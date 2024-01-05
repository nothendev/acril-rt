# acril-rt - a small single-threaded runtime for Acril

Features:
- Tiny - less than 300 lines of code
- Fast - runs on an efficient event loop powered by Tokio
- Safe - `#![forbid(unsafe_code)]` here and in Acril

## Examples

```rs
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

// Use a single thread for the runtime
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let runtime = acril_rt::Runtime::new();
    let addr = runtime.spawn(Pinger { count: 0 }).await;

    loop {
        let pong = addr.send(Ping).await.unwrap();
        println!("pong #{}", pong.0);
    }
}
```
