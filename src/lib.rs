#![forbid(unsafe_code)]
#![allow(incomplete_features)]
#![feature(return_type_notation)]

use std::{
    any::{Any, TypeId},
    collections::HashMap,
    marker::PhantomData,
    pin::Pin,
    sync::Arc, future::IntoFuture,
};

use acril::{Future, Handler, Service};
use tokio::{
    sync::{
        mpsc::{self, unbounded_channel, Sender, UnboundedReceiver, UnboundedSender},
        oneshot,
    },
    task::JoinHandle,
};

type Unknown = Box<dyn Any + Sync + Send + 'static>;
type AnyProc = Unknown;
type AnyMsg = Box<dyn Any + Sync + Send + 'static>;

type ProcessRunner = Arc<
    dyn Fn(AnyProc, AddrErased, AnyMsg) -> Pin<Box<dyn Future<Output = AnyProc> + Send>>
        + Send
        + Sync
        + 'static,
>;

type ExitSignalSender = Box<
    dyn FnOnce(Unknown) -> Pin<Box<dyn Future<Output = Unknown> + Send>> + Send + Sync + 'static,
>;

type MessageSender = UnboundedSender<(u32, ProcessRunner, AnyMsg)>;
type Subscribers = HashMap<TypeId, Vec<(ProcessRunner, MessageSender)>>;

pub struct Addr<S: Service> {
    erased: AddrErased,
    phantom: PhantomData<S>,
}

struct AddrErased {
    id: u32,
    runner_tx: MessageSender,
}

impl<S: Service<Context = Context<S>> + Send + Sync + 'static> Addr<S> {
    pub async fn send<M>(&self, msg: M) -> Result<S::Response, S::Error>
    where
        S: Handler<M, call(): Send>,
        M: 'static + Send + Sync,
        S::Context: Send + Sync,
        S::Error: Send + Sync,
        S::Response: Send + Sync,
    {
        let (tx, mut rx) = mpsc::channel(1);

        self.erased
            .runner_tx
            .send((self.erased.id, message_handler(Some(tx)), Box::new(msg)))
            .unwrap();

        rx.recv().await.unwrap()
    }
}

pub struct Context<S: Service<Context = Self>> {
    addr: Addr<S>,
}

fn message_handler<S, M>(responder: Option<Sender<Result<S::Response, S::Error>>>) -> ProcessRunner
where
    S: 'static + Send + Sync + Handler<M, call(): Send, Context = Context<S>>,
    M: 'static + Send + Sync,
    S::Error: Send + Sync,
    S::Response: Send + Sync,
{
    Arc::new(move |actor, erased, msg| {
        let mut proc = actor.downcast::<S>().unwrap();
        let msg = msg.downcast::<M>().unwrap();
        let responder = responder.clone();

        Box::pin(async move {
            let res = proc
                .call(
                    *msg,
                    &mut Context {
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

async fn event_loop(
    mut new_processes: UnboundedReceiver<(AnyProc, oneshot::Sender<AddrErased>)>,
) {
    let mut processes: HashMap<u32, AnyProc> = HashMap::new();
    let mut count: u32 = 0;
    let (message_sender, mut messages): (MessageSender, _) = unbounded_channel();

    loop {
        tokio::select! { biased;
            Some((new_proc, pid_sender)) = new_processes.recv() => {
                let id = count;
                processes.insert(id, new_proc);
                count += 1;
                let _ = pid_sender.send(AddrErased { id, runner_tx: message_sender.clone() });
            }
            Some((id, runner, msg)) = messages.recv() => {
                let proc = processes.remove(&id).unwrap();
                let proc = runner(proc, AddrErased { id, runner_tx: message_sender.clone() }, msg).await;
                processes.insert(id, proc);
            }
            else => break
        }
    }
}

pub struct Runtime {
    new_processes: UnboundedSender<(AnyProc, oneshot::Sender<AddrErased>)>,
    join: JoinHandle<()>,
}

impl Runtime {
    pub fn new() -> Self {
        let (proc_sender, proc_recv) = unbounded_channel();

        Self {
            new_processes: proc_sender,
            join: tokio::spawn(event_loop(proc_recv)),
        }
    }

    pub async fn spawn<S: Service<Context = Context<S>> + Send + Sync + 'static>(
        &self,
        service: S,
    ) -> Addr<S> {
        let (addr_send, addr_recv) = oneshot::channel();

        self.new_processes.send((Box::new(service), addr_send)).unwrap();

        Addr { erased: addr_recv.await.unwrap(), phantom: PhantomData }
    }
}

impl IntoFuture for Runtime {
    type Output = <JoinHandle<()> as Future>::Output;
    type IntoFuture = JoinHandle<()>;

    fn into_future(self) -> Self::IntoFuture {
        self.join
    }
}
