enum Printer {
    ToString(String),
    ToStdout,
}

impl acril::Service for Printer {
    type Context = acril_rt::Context<Self>;
    type Error = ();
}
impl acril::Handler<String> for Printer {
    type Response = ();
    async fn call(
        &mut self,
        request: String,
        _cx: &mut Self::Context,
    ) -> Result<Self::Response, Self::Error> {
        match self {
            Self::ToStdout => println!("{request}"),
            Self::ToString(s) => s.push_str(&request),
        };
        Ok(())
    }
}

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

    async fn call(&mut self, _ping: Ping, cx: &mut Self::Context) -> Result<Pong, Self::Error> {
        cx.singleton::<Printer>().await.send(format!("ping #{}", self.count)).await?;

        self.count += 1;

        Ok(Pong(self.count))
    }
}

fn main() -> std::thread::Result<()> {
    let arbiter = acril_rt::Arbiter::new();
    let runtime = acril_rt::Runtime::new_in(&arbiter);

    arbiter.spawn(async move {
        let printer = runtime.spawn(Printer::ToStdout).await;
        let addr = runtime
            .spawn(Pinger {
                count: 0,
            })
            .await;

        printer.do_send("Liftoff!".to_string());

        loop {
            let pong = addr.send(Ping).await.unwrap();
            printer.send(format!("pong #{}", pong.0)).await.unwrap();
        }
    });

    arbiter.join()
}
