use super::*;

#[cfg(test)]
use serial_test_derive::serial;

use byteorder::{LittleEndian, WriteBytesExt};

pub type GCResult<T> = Result<T, sys::EGCResults>;

pub struct GC<Manager> {
    pub(crate) gc: *mut sys::ISteamGameCoordinator,
    pub(crate) _inner: Arc<Inner<Manager>>,
}

unsafe impl <Manager> Send for GC<Manager> {}
unsafe impl <Manager> Sync for GC<Manager> {}
impl <Manager> Clone for GC<Manager> {
    fn clone(&self) -> Self {
        Self {
            gc: self.gc,
            _inner: self._inner.clone()
        }
    }
}


/// Represents the properties of a received message
#[derive(Debug)]
pub struct RecvMessageProperties {
    /// Raw type of the message. If the MSB is set, this message is a proto message
    msg_type: u32,

    /// Te size of the message buffer
    msg_size: u32
}

#[repr(C)]
struct IGameCoordinator_vtable {
    /// virtual EGCResults SendMessage( uint32 unMsgType, const void *pubData, uint32 cubData ) = 0;
    #[cfg(all(windows, target_pointer_width = "32"))]
    send_message: extern "thiscall" fn(c: *mut sys::ISteamGameCoordinator, unMsgType: u32, pubData: *const u8, cubData: u32) -> sys::EGCResults,
    #[cfg(not(windows))]
    send_message: extern "C" fn(c: *mut sys::ISteamGameCoordinator, unMsgType: u32, pubData: *const u8, cubData: u32) -> sys::EGCResults,

    /// virtual bool IsMessageAvailable( uint32 *pcubMsgSize ) = 0;
    #[cfg(all(windows, target_pointer_width = "32"))]
    is_message_available: extern "thiscall" fn(c: *mut sys::ISteamGameCoordinator, pcubMsgSize: *mut u32) -> bool,
    #[cfg(not(windows))]
    is_message_available: extern "C" fn(c: *mut sys::ISteamGameCoordinator, pcubMsgSize: *mut u32) -> bool,

    /// virtual EGCResults RetrieveMessage( uint32 *punMsgType, void *pubDest, uint32 cubDest, uint32 *pcubMsgSize ) = 0;
    #[cfg(all(windows, target_pointer_width = "32"))]
    retrieve_message: extern "thiscall" fn(c: *mut sys::ISteamGameCoordinator, punMsgType: *mut u32, pubDest: *mut u8, cubDest: u32, pcubMsgSize: *mut u32) -> sys::EGCResults,
    #[cfg(not(windows))]
    retrieve_message: extern "C" fn(c: *mut sys::ISteamGameCoordinator, punMsgType: *mut u32, pubDest: *mut u8, cubDest: u32, pcubMsgSize: *mut u32) -> sys::EGCResults,
}

impl <Manager> GC<Manager> {
    /// Get the vtable for ISteamGameCoordinator
    fn get_vtable(&self) -> *const IGameCoordinator_vtable
    {
        unsafe {
            debug_assert!(!self.gc.is_null());
            let vtable_ref = self.gc as *const *const IGameCoordinator_vtable;
            *vtable_ref
        }
    }

    /// Send an encoded message to the GC. Returns Ok(()) if the message was sent successfully, otherwise Err(EGCResults)
    /// Automatically prepends the 8-byte GC header
    pub fn send_message(&self, msg_type: u32, msg_data: &[u8]) -> GCResult<()>
    {
        // Write out the header to a temp buffer before sending off
        let mut temp_vec: Vec<u8> = Vec::with_capacity(msg_data.len() + 8);
        temp_vec.write_u32::<LittleEndian>(msg_type).unwrap();
        temp_vec.write_u32::<LittleEndian>(msg_data.len() as u32).unwrap();
        temp_vec.extend_from_slice(msg_data);

        unsafe {
            // Call into ISteamGameCoordinator to send the message
            let res = ((*self.get_vtable()).send_message)(self.gc, msg_type, temp_vec.as_ptr(), temp_vec.len() as u32);
            if res == sys::EGCResults::k_EGCResultOK {
                Ok(())
            } else {
                Err(res)
            }
        }
    }

    /// Receive a message at the head of the queue. If Ok(), a message is written to out_vec and the properties of the message are returned.
    /// If the vector does not have enough capacity to hold the message, the vector is grown to fit the message and retried.
    /// It is preferable to use the message size returned from the GCMessageAvailable callback to reduce allocations
    pub fn recv_message(&self, out_vec: &mut Vec<u8>) -> GCResult<RecvMessageProperties>
    {
        // if not initialized, resize to a 'reasonable' size
        if out_vec.capacity() == 0
        {
            out_vec.reserve(4096);
        }

        // Try to write out to the current vector
        unsafe {
            let mut props = RecvMessageProperties {
                msg_type: 0,
                msg_size: 0
            };

            // keep trying until we get the buffer size right
            loop {
                // attempt to receive the message
                let res = ((*self.get_vtable()).retrieve_message)(self.gc, &mut props.msg_type, out_vec.as_mut_ptr(), out_vec.capacity() as u32, &mut props.msg_size);

                // if we got the message into the vector
                if res == sys::EGCResults::k_EGCResultOK {
                    // ensure we return the length property of the vector properly
                    out_vec.set_len(props.msg_size as usize);

                    // and return the properties of the message
                    return Ok(props)
                } else {
                    // otherwise, did we pass a vector that was too small?
                    if res == sys::EGCResults::k_EGCResultBufferTooSmall {
                        // reserve more space and try again
                        // notice that RetrieveMessage knows the correct size of the message but will NOT return you
                        // that value until you pass a buffer large enough... so we just have to keep growing and hope it works
                        // stupid stupid stupid api design
                        out_vec.reserve(out_vec.capacity());
                    } else {
                        // otherwise we actually have a real error
                        return Err(res)
                    }
                }
            }
        }
    }

    /// Is there a gc message available in the queue? Returns Some(message_size).
    pub fn is_message_available(&self) -> Option<u32> {
        unsafe {
            let mut out_int: u32 = 0;
            let res = ((*self.get_vtable()).is_message_available)(self.gc, &mut out_int);
            if res {
                Some(out_int)
            }else{
                None
            }
        }
    }
}

const CALLBACK_BASE_ID: i32 = 1700;

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct GCMessageAvailable {
    pub message_size: u32,
}

unsafe impl Callback for GCMessageAvailable {
    const ID: i32 = CALLBACK_BASE_ID + 1;
    const SIZE: i32 = ::std::mem::size_of::<sys::GCMessageAvailable_t>() as i32;

    unsafe fn from_raw(raw: *mut libc::c_void) -> Self {
        let val = &mut *(raw as *mut sys::GCMessageAvailable_t);
        GCMessageAvailable {
            message_size: val.m_nMessageSize
        }
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct GCMessageFailed {
}

unsafe impl Callback for GCMessageFailed {
    const ID: i32 = CALLBACK_BASE_ID + 2;
    const SIZE: i32 = ::std::mem::size_of::<sys::GCMessageFailed_t>() as i32;

    unsafe fn from_raw(_raw: *mut libc::c_void) -> Self {
        GCMessageFailed {
        }
    }
}

#[test]
#[serial]
fn test() {
    // ensure we can connect to a client and create a gc interface
    let (client, single) = Client::init().unwrap();
    let gc = client.gc();
    assert!(!gc.gc.is_null());

    println!("ISteamGameCoordinator: {:p}", gc.gc);

    // ensure at is_message_available is working correctly
    let res = gc.is_message_available();
    assert_eq!(None, res);

    // register the callback and read messages as they come in
    let gc_clone = gc.clone();
    let _cb = client.register_callback( move |v: GCMessageAvailable| {
        println!("Queued pending gc message: {}", v.message_size);

        // receive the message from the queue and ensure
        let mut dummy: Vec<u8> = Vec::with_capacity(v.message_size as usize);
        let res = gc_clone.recv_message(&mut dummy);
        if let Ok(x) = res {
            println!("--- Received Message ---");
            println!("message_type: {}", x.msg_type & 0x7FFFFFFF);
            println!("message_size: {}", x.msg_size);
            println!("------------------------");
        } else {
            panic!("GC error on receive, returned: {}", res.unwrap_err() as u32);
        }
    });

    // run some callbacks
    for _ in 0 .. 20 {
        single.run_callbacks();
        ::std::thread::sleep(::std::time::Duration::from_millis(50));
    }

    // send a GC hello
    let res = gc.send_message(0x80000000 + 4006, &[]);
    dbg!(res);

    // run some more callbacks waiting for the GC welcome
    for _ in 0 .. 20 {
        single.run_callbacks();
        ::std::thread::sleep(::std::time::Duration::from_millis(50));
    }

}