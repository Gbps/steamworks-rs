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

/// Describes an entry in the message queue
#[derive(Debug)]
pub struct GCMessageQueueEntry {
    /// Properties of the message, such as length and type
    pub props: RecvMessageProperties,

    /// The buffer containing the actual message's contents
    pub buffer: Vec<u8>,

    /// If true, an error happened while receiving and the other fields are not valid
    error: bool
}

/// A high level message queue to assist in receiving GC messages easily
pub struct GCMessageQueue<Manager> {
    /// A reference to the gc instance
    client: Client<Manager>,

    /// A reference to the callback registered for the message queue
    callback: Option<CallbackHandle<Manager>>,

    /// Hashmap which dispatches message id types to callbacks that service them
    packet_callbacks: Arc<Mutex<HashMap<u32, Box<dyn FnMut(GCMessageQueueEntry) + Send + 'static>>>>
}

/// Handle representing a callback that will be dropped automatically
pub struct PktCallbackHandle {
    msg_type: u32,
    packet_callbacks: Arc<Mutex<HashMap<u32, Box<dyn FnMut(GCMessageQueueEntry) + Send + 'static>>>>
}

/// Implements automatic drop for install_callback_handle
impl Drop for PktCallbackHandle {
    fn drop(&mut self) {
        println!("Dropped!");
        match self.packet_callbacks.lock()
        {
            // remove the callback from the table if the reference is valid
            Ok(mut cbs) => { cbs.remove(&self.msg_type); },
            // danging reference to a dropped queue, do nothing.
            Err(_) => {}
        }
        ()
    }
}

impl<Manager: 'static> GCMessageQueue<Manager> where Manager: crate::Manager {
    /// Create a new message queue for a client
    pub fn new(client: Client<Manager>) -> Self {
        // create the queue
        let mut obj = Self {
            client,
            callback: None,
            packet_callbacks: Arc::new(Mutex::new(HashMap::new()))
        };

        // register a callback to service this queue
        obj.start_recv();

        return obj
    }

    /// Begin receiving GC packets into the queue
    fn start_recv(&mut self)
    {
        // get a gc reference
        let gc = self.client.gc();
        let callbacks_ref = self.packet_callbacks.clone();

        let callback = move |v: GCMessageAvailable| {
            // receive the message from the queue and ensure
            let mut buf: Vec<u8> = Vec::with_capacity(v.message_size as usize);
            let res = gc.recv_message(&mut buf);

            // did we receive a message?
            if let Ok(x) = res {
                // okay, let's fire a callback for that type
                if let Ok(mut ht) = callbacks_ref.lock()
                {
                    // do we have a callback entry for this type?
                    let entry = ht.get_mut(&x.msg_type);
                    if entry.is_some() {
                        // call the callback passing the data we just received!
                        let cb = entry.unwrap();
                        cb(GCMessageQueueEntry {
                            props: x,
                            buffer: buf,
                            error: false
                        });
                    }
                }
            } else {
                // we lost gc connection, alert the receiver
                /*
                sender.send(GCMessageQueueEntry {
                    props: RecvMessageProperties{
                        msg_type: 0,
                        msg_size: 0,
                    },
                    buffer: Vec::new(),
                    error: true
                }).unwrap_or_default(
                */

            }
        };

        // register the callback and hold a reference to it
        self.callback = Some(self.client.register_callback(callback));
    }

    /// Add a message to send to the queue. Returns true if the message was successfully queued.
    /// The message data must NOT include the 8-byte header. This is added automatically.
    pub fn send_message(&self, msg_type: u32, msg_data: &[u8]) -> bool
    {
        let gc = self.client.gc();
        let res = gc.send_message(msg_type, msg_data);
        if let Ok(_) = res {
            true
        } else {
            false
        }
    }

    /// Establish a permanent callback function to call whenever a packet of a certain type is received
    ///
    /// Only one callback for a specific message type can exist per queue, if one already exists
    /// the callback is replaced with this one.
    pub fn install_global_callback<C>(&self, msg_type: u32, callback_fn: C)
        where C: FnMut(GCMessageQueueEntry) + Send + 'static
    {
        self.packet_callbacks.lock().unwrap().insert(msg_type, Box::new(callback_fn));
    }

    /// Establish a temporary callback function to call whenever a packet of a certain type is received.
    /// This is used to establish a callback function for a packet type which only exists in the scope
    /// of the returned handle.
    ///
    /// Only one callback for a specific message type can exist per queue, if one already exists
    /// the callback is replaced with this one.
    ///
    /// Returns a handle object which, when dropped, will remove the callback type specified by msg_type.
    /// NOTE: this does not protect against races by other calls to install_handle.
    #[must_use]
    pub fn install_callback<C>(&self, msg_type: u32, callback_fn: C) -> PktCallbackHandle
        where C: FnMut(GCMessageQueueEntry) + Send + 'static
    {
        let mut guard = self.packet_callbacks.lock().unwrap();
        guard.insert(msg_type, Box::new(callback_fn));

        // return a handle which will automatically drop the callback type
        PktCallbackHandle {
            msg_type,
            packet_callbacks: self.packet_callbacks.clone()
        }
    }

    /// Removes a previously registered callback by its message type
    /// If no callback was registered already for that type, returns false. Otherwise, returns true.
    pub fn remove_callback(&self, msg_type: u32) -> bool
    {
        self.packet_callbacks.lock().unwrap().remove(&msg_type).is_some()
    }
}

#[derive(Clone, Debug)]
pub struct GCMessageAvailable {
    pub message_size: u32,
}

const CALLBACK_BASE: i32 = 1700;

/// Callback object to track when a GC message is available in the queue
unsafe impl Callback for GCMessageAvailable {
    const ID: i32 = CALLBACK_BASE + 1;
    const SIZE: i32 = ::std::mem::size_of::<sys::GCMessageAvailable_t>() as i32;

    unsafe fn from_raw(raw: *mut libc::c_void) -> Self {
        let val = &mut *(raw as *mut sys::GCMessageAvailable_t);
        GCMessageAvailable {
            message_size: val.m_nMessageSize
        }
    }
}

#[derive(Clone, Debug)]
pub struct GCMessageFailed {
}

/// Callback object to track when a GC message failed to send/recv
unsafe impl Callback for GCMessageFailed {
    const ID: i32 = CALLBACK_BASE + 2;
    const SIZE: i32 = ::std::mem::size_of::<sys::GCMessageFailed_t>() as i32;

    unsafe fn from_raw(_raw: *mut libc::c_void) -> Self {
        GCMessageFailed {
        }
    }
}

#[cfg(test)]
const CLIENT_HELLO_MESSAGE_ID: u32 = 0x80000000 + 4006;
#[cfg(test)]
const CLIENT_WELCOME_MESSAGE_ID: u32 = 0x80000000 + 4004;

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
    let res = gc.send_message(CLIENT_HELLO_MESSAGE_ID, &[]);
    dbg!(res.unwrap());

    // run some more callbacks waiting for the GC welcome
    for _ in 0 .. 20 {
        single.run_callbacks();
        ::std::thread::sleep(::std::time::Duration::from_millis(50));
    }

}

#[test]
#[serial]
fn test_queue() {
    // ensure we can connect to a client and create a gc interface
    let (client, single) = Client::init().unwrap();

    // let steam warm up
    ::std::thread::sleep(::std::time::Duration::from_millis(1000));

    // notify us when the message is successfully received in the other thread
    let (sender, receiver) = std::sync::mpsc::channel::<bool>();

    // spawn a thread to do send/recv operations
    let _ = std::thread::spawn(move || {
        let queue = GCMessageQueue::new(client.clone());

        // install a callback to respond for the welcome packet
        queue.install_global_callback(CLIENT_WELCOME_MESSAGE_ID, move |pkt| {
            dbg!(&pkt.props);
            // tell the test thread that we were successful
            sender.send(true).unwrap();
        });

        // send a k_EMsgGCClientHello
        dbg!(queue.send_message(CLIENT_HELLO_MESSAGE_ID, &[]));

        // keep the thread and queue alive
        ::std::thread::sleep(::std::time::Duration::from_millis(3000));
    });

    // loop performing callbacks here
    for _ in 0 .. 50 {
        single.run_callbacks();
        ::std::thread::sleep(::std::time::Duration::from_millis(50));

        // did the packet successfully get received?
        if let Ok(_) = receiver.try_recv()
        {
            // successfully received the message, exit test
            return
        }
    }

    panic!("Did not receive GC welcome packet.");
}

#[test]
#[serial]
fn test_callback_handle() {
    // ensure we can connect to a client and create a gc interface
    let (client, single) = Client::init().unwrap();

    // let steam warm up
    ::std::thread::sleep(::std::time::Duration::from_millis(1000));

    // notify us when the message is successfully received in the other thread
    let (sender, receiver) = std::sync::mpsc::channel::<bool>();

    // spawn a thread to do send/recv operations
    let other_thrd = std::thread::spawn(move || {
        let queue = GCMessageQueue::new(client.clone());

        {
            // install a callback to respond for the welcome packet
            let _hndl = queue.install_callback(CLIENT_WELCOME_MESSAGE_ID, move |pkt| {
                dbg!(&pkt.props);
                // tell the test thread that we were successful
                sender.send(true).unwrap();
            });

            // send a k_EMsgGCClientHello
            dbg!(queue.send_message(CLIENT_HELLO_MESSAGE_ID, &[]));

            // keep the thread and queue alive
            ::std::thread::sleep(::std::time::Duration::from_millis(1000));
        }

        // ensure our callback was dropped
        assert_eq!(queue.packet_callbacks.lock().unwrap().len(), 0);
    });

    // loop performing callbacks here
    for _ in 0 .. 50 {
        single.run_callbacks();
        ::std::thread::sleep(::std::time::Duration::from_millis(50));

        // did the packet successfully get received?
        if let Ok(_) = receiver.try_recv()
        {
            // successfully received the message, exit test
            other_thrd.join();
            return
        }
    }

    panic!("Did not receive GC welcome packet.");
}