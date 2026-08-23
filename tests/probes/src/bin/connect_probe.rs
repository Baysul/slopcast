use std::time::Duration;

fn try_connect(label: &str, rt: &tokio::runtime::Runtime, token: &str) {
    rt.block_on(async {
        let connect = async {
            let (room, _events) = livekit::Room::connect(
                "ws://127.0.0.1:17880",
                token,
                livekit::RoomOptions::default(),
            )
            .await
            .map_err(|e| format!("connect: {e}"))?;
            println!("[probe:{label}] room connected");
            room.close().await.ok();
            Ok::<(), String>(())
        };
        match tokio::time::timeout(Duration::from_secs(15), connect).await {
            Ok(Ok(())) => println!("[probe:{label}] OK"),
            Ok(Err(e)) => println!("[probe:{label}] ERROR: {e}"),
            Err(_) => println!("[probe:{label}] TIMEOUT after 15s"),
        }
    });
}

fn main() {
    let token = livekit_api::access_token::AccessToken::with_api_key("devkey", "secret")
        .with_identity("probe")
        .with_grants(livekit_api::access_token::VideoGrants {
            room_join: true,
            room: "probe-room".into(),
            can_publish: true,
            ..Default::default()
        })
        .to_jwt()
        .unwrap();
    let multi = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    try_connect("multi-thread", &multi, &token);
}
