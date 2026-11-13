//! A publisher and a viewer connect over 127.0.0.1, with every signal passing
//! through the same JSON the TeamSpeak relay would carry.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use tokio::sync::mpsc::Receiver;
use ts6_stream::{Publisher, Signal, Viewer};

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Annex-B access unit: SPS, PPS and an IDR slice with dummy payload bytes.
/// Enough for the RTP packetizer; it is not a decodable picture.
fn synthetic_sample() -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f, 0xda, 0x01]);
    data.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80]);
    data.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88, 0x84]);
    data.extend(std::iter::repeat_n(0x5a, 3000));
    data
}

/// Everything waiting in `from`, as the wire JSON the relay would carry.
async fn drain(from: &mut Receiver<Signal>) -> Vec<String> {
    let mut wire = Vec::new();
    while let Ok(Some(signal)) = tokio::time::timeout(Duration::from_millis(10), from.recv()).await
    {
        wire.push(signal.to_json());
    }
    wire
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn viewer_receives_published_video_over_loopback() {
    let mut publisher = Publisher::new(LOCALHOST).await.expect("publisher");
    let mut viewer = Viewer::new(LOCALHOST).await.expect("viewer");

    let offer = publisher.offer().await.expect("offer");
    let answer = viewer.accept_offer(&offer).await.expect("answer");
    let relayed = Signal::parse(&Signal::Answer(answer).to_json()).unwrap();
    publisher.apply(relayed).await.expect("answer applies");

    let sample = synthetic_sample();
    for _ in 0..300 {
        for json in drain(publisher.signals()).await {
            viewer
                .apply(Signal::parse(&json).unwrap())
                .await
                .expect("viewer applies candidate");
        }
        for json in drain(viewer.signals()).await {
            publisher
                .apply(Signal::parse(&json).unwrap())
                .await
                .expect("publisher applies candidate");
        }
        if publisher.is_connected() {
            publisher
                .send_sample(sample.clone(), Duration::from_millis(33))
                .await
                .expect("send sample");
        }
        if viewer.packets() >= 30 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(33)).await;
    }

    assert!(
        viewer.packets() >= 30,
        "viewer saw {} packets",
        viewer.packets()
    );
    publisher.close().await;
    viewer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_is_sent_before_the_viewer_answers() {
    let publisher = Publisher::new(LOCALHOST).await.expect("publisher");
    publisher.offer().await.expect("offer");
    let refused = publisher
        .send_sample(synthetic_sample(), Duration::from_millis(33))
        .await;
    assert!(refused.is_err());
    publisher.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_candidate_that_beats_the_answer_is_held_not_lost() {
    let publisher = Publisher::new(LOCALHOST).await.expect("publisher");
    let mut viewer = Viewer::new(LOCALHOST).await.expect("viewer");
    let offer = publisher.offer().await.expect("offer");
    let answer = viewer.accept_offer(&offer).await.expect("answer");

    // The viewer's candidates reach the publisher before its answer does.
    let early = drain(viewer.signals()).await;
    assert!(!early.is_empty(), "the viewer gathered no candidate");
    for json in &early {
        publisher
            .apply(Signal::parse(json).unwrap())
            .await
            .expect("an early candidate is accepted");
    }
    publisher
        .apply(Signal::Answer(answer))
        .await
        .expect("the queued candidates apply with the answer");
    publisher.close().await;
    viewer.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_the_publisher_stops_the_flow_without_hanging() {
    let mut publisher = Publisher::new(LOCALHOST).await.expect("publisher");
    let mut viewer = Viewer::new(LOCALHOST).await.expect("viewer");
    let offer = publisher.offer().await.expect("offer");
    let answer = viewer.accept_offer(&offer).await.expect("answer");
    publisher.apply(Signal::Answer(answer)).await.unwrap();
    for _ in 0..300 {
        for json in drain(publisher.signals()).await {
            viewer.apply(Signal::parse(&json).unwrap()).await.unwrap();
        }
        for json in drain(viewer.signals()).await {
            publisher
                .apply(Signal::parse(&json).unwrap())
                .await
                .unwrap();
        }
        if publisher.is_connected() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(33)).await;
    }
    assert!(publisher.is_connected());
    for _ in 0..20 {
        publisher
            .send_sample(synthetic_sample(), Duration::from_millis(33))
            .await
            .expect("send sample");
        tokio::time::sleep(Duration::from_millis(33)).await;
    }
    assert!(viewer.packets() > 0, "no traffic before the close");

    tokio::time::timeout(Duration::from_secs(10), publisher.close())
        .await
        .expect("close returns");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let settled = viewer.packets();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        viewer.packets(),
        settled,
        "packets kept arriving after close"
    );
    tokio::time::timeout(Duration::from_secs(10), viewer.close())
        .await
        .expect("viewer close returns");
}
