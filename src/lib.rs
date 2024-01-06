#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![allow(incomplete_features)]
#![feature(return_type_notation)]

use std::{
    any::{Any, TypeId},
    cell::RefCell,
    collections::HashMap,
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    thread,
};

use acril::{Future, Handler, Service};
use tokio::{
    sync::{
        mpsc::{self, unbounded_channel, Sender, UnboundedReceiver, UnboundedSender},
        oneshot, RwLock,
    },
    task::{JoinHandle, LocalSet},
};

/// A convenience alias to a [`Box`]`<dyn `[`Any`]` + Send + Sync + 'static>`.
type Unknown = Box<dyn Any + Send + Sync + 'static>;
/// A type-erased service.
type AnyProc = Unknown;
/// A type-erased message.
type AnyMsg = Unknown;

/// A type-erased function, which handles a message sent to a service.
type ProcessRunner = Arc<
    dyn Fn(
            AnyProc,
            AddrErased,
            AnyMsg,
            UnboundedSender<RuntimeCommand>,
        ) -> Pin<Box<dyn Future<Output = AnyProc>>>
        + Send
        + Sync,
>;

/// The sender half of a channel for sending messages to running services.
type MessageSender = UnboundedSender<(u32, ProcessRunner, AnyMsg)>;

/// A handle to a running [`Service`], allowing to send messages to it.
pub struct Addr<S: Service> {
    erased: AddrErased,
    phantom: PhantomData<S>,
}

impl<S: Service> Clone for Addr<S> {
    fn clone(&self) -> Self {
        Self {
            erased: self.erased.clone(),
            phantom: PhantomData,
        }
    }
}

/// An erased service address, containing an ID and message sender.
#[derive(Clone)]
struct AddrErased {
    id: u32,
    runner_tx: MessageSender,
}

impl<S: Service<Context = Context<S>>> Addr<S> {
    /// Send a message to the service pointed to by this address.
    pub async fn send<M>(&self, msg: M) -> Result<S::Response, S::Error>
    where
        S: Handler<M> + Send + Sync + 'static,
        M: 'static + Send + Sync,
        S::Error: Send + Sync,
        S::Response: Send + Sync,
    {
        let (tx, mut rx) = mpsc::channel(1);

        self.erased
            .runner_tx
            .send((
                self.erased.id,
                message_handler::<S, M>(Some(tx)),
                Box::new(msg),
            ))
            .unwrap();

        rx.recv().await.unwrap()
    }

    /// Just send a message, without waiting for a response.
    ///
    /// Be aware that by using this function you allow desynchonization to happen, as this function
    /// doesn't wait for the response.
    ///
    /// If you don't want desyncs to happen, use [`send`](Self::send).
    pub fn do_send<M>(&self, msg: M)
    where
        S: Handler<M> + Send + Sync + 'static,
        M: 'static + Send + Sync,
        S::Error: Send + Sync,
        S::Response: Send + Sync,
    {
        self.erased
            .runner_tx
            .send((self.erased.id, message_handler::<S, M>(None), Box::new(msg)))
            .unwrap()
    }
}

/// A service context.
///
/// Any service that is spawned onto a [`Runtime`] needs to have this type as context.
pub struct Context<S: Service<Context = Self>> {
    addr: Addr<S>,
    commands: UnboundedSender<RuntimeCommand>,
}

impl<S: Service<Context = Self>> Context<S> {
    /// Get the address of this service.
    ///
    /// Beware that if you use this to send a message to yourself (with [`Addr::send`]), you will
    /// encounter a deadlock because to handle a message you need send a message, which just makes
    /// the message handler never finish.
    pub fn this(&self) -> Addr<S> {
        self.addr.clone()
    }

    /// Spawn a future onto the current arbiter.
    pub fn spawn<F: Future + 'static>(&self, fut: F) -> JoinHandle<F::Output> {
        tokio::task::spawn_local(fut)
    }

    /// Try to retrieve the address of a singleton service `T`.
    /// If there are more than one or no instances of `T` running,
    /// this method will return a [`SingletonError`].
    pub async fn try_singleton<T: Service<Context = Context<T>> + Any>(
        &self,
    ) -> Result<Addr<T>, SingletonError> {
        let (addr_send, addr_recv) = oneshot::channel();

        let _ = self.commands.send(RuntimeCommand::GetAddrOf {
            ty: TypeId::of::<T>(),
            addr: addr_send,
        });

        addr_recv
            .await
            .expect("runtime has been shut down")
            .map(|erased| Addr {
                erased,
                phantom: PhantomData,
            })
    }

    /// Retrieve an address to the singleton service `T`.
    /// This method panics if the service wasn't running
    /// or there were more than one instance of it.
    pub async fn singleton<T: Service<Context = Context<T>> + Any>(&self) -> Addr<T> {
        self.try_singleton().await.unwrap_or_else(|error| {
            panic!(
                "A singleton {T} was not available: {error:?}",
                T = std::any::type_name::<T>()
            )
        })
    }
}

/// Make a new message handler. This takes a responder, which is an optional channel for sending
/// the response of the service. A context is created inline to avoid storing another thing inside
/// of the event loop.
fn message_handler<S, M>(responder: Option<Sender<Result<S::Response, S::Error>>>) -> ProcessRunner
where
    S: 'static + Handler<M, Context = Context<S>> + Send + Sync,
    M: 'static + Send + Sync,
    S::Error: Send + Sync,
    S::Response: Send + Sync,
{
    Arc::new(move |actor, erased, msg, commands| {
        let mut proc = actor.downcast::<S>().unwrap();
        let msg = msg.downcast::<M>().unwrap();
        let responder = responder.clone();

        Box::pin(async move {
            let res = proc
                .call(
                    *msg,
                    &mut Context {
                        commands,
                        addr: Addr {
                            erased,
                            phantom: PhantomData::<S>,
                        },
                    },
                )
                .await;

            if let Some(responder) = &responder {
                responder.send(res).await.ok();
            }

            proc as Unknown
        })
    })
}

thread_local! {
    /// A handle to the arbiter that the current task is running on.
    static HANDLE: RefCell<Option<ArbiterHandle>> = RefCell::new(None);
}

/// An arbiter is a single-threaded event loop, allowing users to spawn tasks onto it.
pub struct Arbiter {
    thread_handle: thread::JoinHandle<()>,
    // store a handle instead of the tx itself so we can make Arbiter deref to the handle
    // to not copy-paste spawn and stop methods
    arb: ArbiterHandle,
}

/// A command to an arbiter.
enum ArbiterCommand {
    /// Stop the arbiter
    Stop,
    /// Execute a future.
    Execute(Pin<Box<dyn Future<Output = ()> + Send>>),
}

impl Arbiter {
    async fn runner(mut rx: UnboundedReceiver<ArbiterCommand>) {
        // clever trick: the loop ends if there is a `None` or `Some(Stop)`
        while let Some(ArbiterCommand::Execute(fu)) = rx.recv().await {
            tokio::task::spawn_local(fu);
        }
    }

    /// Get a handle to the arbiter that the current task is running in.
    /// If the arbiter is not available, this function panics. If you don't want a panic, use
    /// [`Self::try_current`], which returns an [`Option`]`<`[`ArbiterHandle`]`>`.
    pub fn current() -> ArbiterHandle {
        Self::try_current().expect("no arbiter was available")
    }

    /// Get a handle to the arbiter that the current task is running in.
    /// If the arbiter is not available, this returns [`None`].
    pub fn try_current() -> Option<ArbiterHandle> {
        HANDLE.with_borrow(|x| x.as_ref().map(|a| ArbiterHandle { tx: a.tx.clone() }))
    }

    /// Get a handle to this arbiter.
    pub fn handle(&self) -> &ArbiterHandle {
        &self.arb
    }

    pub fn new() -> Self {
        Self::with_tokio_rt(|| {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
        })
    }

    pub fn with_tokio_rt(factory: impl Fn() -> tokio::runtime::Runtime + Send + 'static) -> Self {
        let (tx, rx) = unbounded_channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();

        Self {
            thread_handle: thread::Builder::new()
                .name("acril-rt-arbiter".to_string())
                .spawn({
                    let tx = tx.clone();
                    move || {
                        let tokio = factory();
                        let local_set = LocalSet::new();
                        let _guard = local_set.enter();

                        // "register" the arbiter
                        HANDLE.set(Some(ArbiterHandle { tx }));

                        ready_tx.send(()).unwrap();

                        tokio.block_on(local_set.run_until(Self::runner(rx)));

                        // de-"register" the arbiter
                        HANDLE.set(None);
                    }
                })
                .unwrap(),
            arb: ArbiterHandle {
                tx: {
                    ready_rx.recv().unwrap();
                    tx
                },
            },
        }
    }

    pub fn join(self) -> std::thread::Result<()> {
        self.thread_handle.join()
    }
}

/// A handle to an arbiter, allowing to spawn futures onto the arbiter or stop it.
#[derive(Clone)]
pub struct ArbiterHandle {
    tx: UnboundedSender<ArbiterCommand>,
}

impl std::ops::Deref for Arbiter {
    type Target = ArbiterHandle;
    fn deref(&self) -> &Self::Target {
        &self.arb
    }
}

impl ArbiterHandle {
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) -> bool {
        self.tx
            .send(ArbiterCommand::Execute(Box::pin(future)))
            .is_ok()
    }

    pub fn stop(&self) -> bool {
        self.tx.send(ArbiterCommand::Stop).is_ok()
    }
}

/// A command sent to a runtime.
enum RuntimeCommand {
    /// Spawn a service.
    Process {
        ty: TypeId,
        proc: AnyProc,
        addr: oneshot::Sender<AddrErased>,
    },
    /// Get the address of a singleton service with said [`TypeId`].
    GetAddrOf {
        ty: TypeId,
        addr: oneshot::Sender<Result<AddrErased, SingletonError>>,
    },
}

/// The main component of `acril_rt` - the runtime. It stores the running services
/// and handles their lifecycle and messages sent to them.
pub struct Runtime {
    command_sender: UnboundedSender<RuntimeCommand>,
}

impl Runtime {
    pub fn new() -> Self {
        let (command_sender, command_recv) = unbounded_channel();

        let _ = tokio::task::spawn_local(Self::event_loop(command_recv, command_sender.clone()));

        Self { command_sender }
    }

    pub fn new_in(arbiter: &ArbiterHandle) -> Self {
        let (command_sender, command_recv) = unbounded_channel();

        arbiter.spawn(Self::event_loop(command_recv, command_sender.clone()));

        Self { command_sender }
    }

    pub async fn spawn<S: Service<Context = Context<S>> + Send + Sync + 'static>(
        &self,
        service: S,
    ) -> Addr<S> {
        let (addr_send, addr_recv) = oneshot::channel();

        self.command_sender
            .send(RuntimeCommand::Process {
                ty: service.type_id(),
                proc: Box::new(service),
                addr: addr_send,
            })
            .unwrap();

        Addr {
            erased: addr_recv.await.unwrap(),
            phantom: PhantomData,
        }
    }

    async fn event_loop(
        mut commands: UnboundedReceiver<RuntimeCommand>,
        commands_sender: UnboundedSender<RuntimeCommand>,
    ) {
        let processes: Arc<RwLock<HashMap<u32, (TypeId, AnyProc)>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let mut count: u32 = 0;
        let (message_sender, mut messages): (MessageSender, _) = unbounded_channel();

        loop {
            tokio::select! {
                Some((id, runner, msg)) = messages.recv() => {
                            let (ty, proc) = processes.write().await.remove(&id).unwrap();

                            tokio::task::spawn_local({ let processes = processes.clone(); let commands_sender = commands_sender.clone(); let runner_tx = message_sender.clone(); async move {
                                let proc = runner(proc, AddrErased { id, runner_tx }, msg, commands_sender).await;
                                processes.write().await.insert(id, (ty, proc));
                            }});
                }
                Some(command) = commands.recv() => {
                    match command {
                        RuntimeCommand::Process { ty, proc, addr } => {
                            let id = count;
                            processes.write().await.insert(id, (ty, proc));
                            count += 1;
                            let _ = addr.send(AddrErased { id, runner_tx: message_sender.clone() });
                        }
                        RuntimeCommand::GetAddrOf { ty, addr } => {
                            let _ = addr.send(
                                only_one(processes.read().await.iter()
                                    .filter(|(_id, (ty_, _))| *ty_ == ty))
                                    .map(|(id, _)| AddrErased { id: *id, runner_tx: message_sender.clone() }).map_err(|e| if e.is_some() { SingletonError::MoreThanOne } else { SingletonError::NotPresent})
                            );
                        }
                    }
                }
                else => break
            }
        }
    }
}

/// An error returned while retrieving a singleton service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SingletonError {
    /// There were more than one instances of the requested service.
    MoreThanOne,
    /// The requested singleton service was not present.
    NotPresent,
}

/// Require the iterator to have only one item, erroring if it has less than one or more than one.
/// If there were more than one items, the next item is present as Err(Some(next_item))
pub fn only_one<I: Iterator>(mut iter: I) -> Result<I::Item, Option<I::Item>> {
    let item = iter.next().ok_or(None)?;

    if let Some(next) = iter.next() {
        Err(Some(next))
    } else {
        Ok(item)
    }
}

/// `use acril_rt::prelude::*;` to import the commonly used types.
pub mod prelude {
    #[doc(no_inline)]
    pub use acril::{self, Handler, Service};
    pub use crate::{Addr, Arbiter, Context, Runtime};
}
