#![allow(unused_variables)]
use stomp::header::{ContentType, Header};
use stomp::subscription::AckOrNack::Ack;
use stomp::frame::Frame;
use std::thread;
use std::process;
use smol::block_on;
use std::io::Error;
use futures::StreamExt;
use stomp::session::SessionEvent;
use std::time::Duration;

const TOTAL_MESSAGES : u64 = 1;
const INTERVAL : u64 = 10;

async fn run() -> Result<(), Error> {
    let destination = "/queue/ben";
    let mut messages_received: u64 = 0;

    let sender = smol::spawn(async move {
        let mut messages_sent: u64 = 0;
        let mut publish_session = stomp::SessionBuilder::new("127.0.0.1", 61613)
            .start()
            .await
            .unwrap();
        loop {
            let sent = publish_session.message(destination, "Modern major general")
                .with(ContentType("text/plain"))
                .send()
                .await;
            messages_sent += 1;
            if messages_sent % INTERVAL == 0 {
                println!("{} messages sent", messages_sent);
            }
            if messages_sent >= TOTAL_MESSAGES {
                println!("Send complete.");
                break;
            }
        }
        let _ = publish_session.disconnect();
        println!("Disconnected.");

    });

    smol::Timer::after(Duration::from_millis(5000)).await;
    println!("Reading");

    let mut subscribe_session = stomp::SessionBuilder::new("127.0.0.1", 61613)
        .start()
        .await?;

    println!("Done connecting");

    let id = subscribe_session
        .subscription(destination)
        .start()
        .await;
    println!("Done subscribing {}", id);

    let recv = subscribe_session.take(1).collect::<Vec<_>>().await;

    //subscribe_session.disconnect().await;

    //let recv = recv.await;


    println!("Recv: {:?}", recv);
    Ok(())
}

fn main() {
    env_logger::init();
    block_on(async {
        run().await;
    });
    println!("main done");

}
