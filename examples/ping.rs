struct Pinger {
    count: u32,
}

struct Ping;
struct Pong(u32);

impl acril::Service for Pinger {
    type Context = acril_rt::Context<Self>;
    type Error = ();
}
impl acril::Handler<Ping> for Pinger {
    type Response = Pong;

    async fn call(&mut self, _ping: Ping, _cx: &mut Self::Context) -> Result<Pong, Self::Error> {
        println!("ping #{}", self.count);

        self.count += 1;

        Ok(Pong(self.count))
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let runtime = acril_rt::Runtime::new();
    let addr = runtime.spawn(Pinger { count: 0 }).await;

    println!("Liftoff!");

    loop {
        let pong = addr.send(Ping).await.unwrap();
        println!("pong #{}", pong.0);
    }
}
