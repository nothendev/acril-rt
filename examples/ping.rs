//! In this example I'm showing off a few different things.
//! - Singletons, with [`acril_rt::Context::singleton`];
//! - Non-void responses, with [`GetOutput`]
//! - "Dependency injection", if you can call it that, based on `#[cfg(debug_assertions)]` with
//! [`Printer`]

// we're toggling between stdout and string based on debug_assertions, so one of them is dead code
// in any case
#[allow(dead_code)]
enum Printer {
    ToString(String),
    ToStdout,
}

impl acril::Service for Printer {
    type Context = acril_rt::Context<Self>;
    type Error = ();
}

struct GetOutput;

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

impl acril::Handler<GetOutput> for Printer {
    type Response = Option<String>;
    async fn call(
        &mut self,
        _get_string: GetOutput,
        _cx: &mut Self::Context,
    ) -> Result<Self::Response, Self::Error> {
        if let Self::ToString(ref s) = self {
            Ok(Some(s.to_owned()))
        } else {
            Ok(None)
        }
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
        cx.singleton::<Printer>()
            .await
            .send(format!("ping #{}", self.count))
            .await?;

        self.count += 1;

        Ok(Pong(self.count))
    }
}
impl acril::Handler<GetOutput> for Pinger {
    type Response = u32;
    async fn call(
        &mut self,
        _request: GetOutput,
        _cx: &mut Self::Context,
    ) -> Result<Self::Response, Self::Error> {
        Ok(self.count)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // make sure the runtime can spawn !Send tasks
    let local_set = tokio::task::LocalSet::new();
    let _guard = local_set.enter();

    let runtime = acril_rt::Runtime::new();

    local_set.run_until(async move {
        #[cfg(debug_assertions)]
        let printer = runtime.spawn(Printer::ToStdout).await;
        #[cfg(not(debug_assertions))]
        let printer = runtime.spawn(Printer::ToString(String::new())).await;
        let pinger = runtime.spawn(Pinger { count: 0 }).await;

        printer.send("Liftoff!".to_string()).await.unwrap();

        for i in 0..10000 {
            let pong = pinger.send(Ping).await.unwrap();

            // i is 0-based and pong is 1-based because the pinger increments before returning
            assert_eq!(pong.0, i + 1);

            printer.send(format!("pong #{}", pong.0)).await.unwrap();
        }

        assert_eq!(pinger.send(GetOutput).await.unwrap(), 10000);
        #[cfg(not(debug_assertions))]
        // assert that the output is Ok(Some(non_empty_string))
        assert!(!printer.send(GetOutput).await.unwrap().unwrap().is_empty());

        printer.send("We're done!".to_string()).await.unwrap();
    }).await;
}
