#[allow(
    clippy::unwrap_used,
    reason = "probe binary: panics on setup errors are the point"
)]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let pid: u32 = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| std::process::id());

    println!("[probe] capturing pid {pid} (as -pid target)");
    let target = native_rust::AudioTarget::Id(-(pid as i32));
    match native_rust::start_audio_capture(&target) {
        Ok(true) => println!("[probe] capture started"),
        Ok(false) => {
            println!("[probe] capture reported false");
            std::process::exit(1);
        }
        Err(e) => {
            println!("[probe] capture error: {e}");
            std::process::exit(1);
        }
    }

    let mut app_active = false;
    let mut linked = false;
    let mut last_print = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let (now_app_active, now_linked) = graph_state();
        if now_app_active != app_active {
            println!(
                "[probe] app node: {}",
                if now_app_active { "PRESENT" } else { "ABSENT" }
            );
            app_active = now_app_active;
        }
        if now_linked != linked {
            println!(
                "[probe] app->capture link: {}",
                if now_linked { "LINKED" } else { "UNLINKED" }
            );
            linked = now_linked;
        }
        if now_linked && last_print.elapsed().as_secs() > 5 {
            println!("[probe] still linked (5s)");
            last_print = std::time::Instant::now();
        }
    }
}

/// Returns (target-app node presence, app→capture link presence) by parsing
/// `pw-dump` JSON with python3. The app→capture link is a Link whose output
/// node is an app Stream/Output/Audio node and input node is the Slopcast
/// capture node.
fn graph_state() -> (bool, bool) {
    let Ok(out) = std::process::Command::new("pw-dump").output() else {
        return (false, false);
    };
    let Ok(json) = std::str::from_utf8(&out.stdout) else {
        return (false, false);
    };
    // Use python3 to parse; the JSON is small enough for argv. Escape quotes.
    let script = r#"
import json, sys
d = json.load(sys.stdin)
nodes = {}
for o in d:
    if o.get('type') == 'PipeWire:Interface:Node':
        p = o.get('info', {}).get('props', {})
        nodes[o['id']] = (p.get('node.name'), p.get('media.class'))
app_present = any(
    n[1] == 'Stream/Output/Audio' and n[0] != 'Slopcast-Window-Audio'
    for n in nodes.values()
)
app_link = False
for o in d:
    if o.get('type') != 'PipeWire:Interface:Link':
        continue
    p = o.get('info', {}).get('props', {})
    outn = nodes.get(int(p.get('link.output.node', 0)), ('', ''))
    inn = nodes.get(int(p.get('link.input.node', 0)), ('', ''))
    if outn[1] == 'Stream/Output/Audio' and inn[0] == 'Slopcast-Window-Audio':
        app_link = True
print(f'{app_present} {app_link}')
"#;
    let Ok(mut child) = std::process::Command::new("python3")
        .args(["-c", script])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
    else {
        return (false, false);
    };
    use std::io::Write;
    let _ = child.stdin.as_mut().expect("stdin").write_all(json.as_bytes());
    let Ok(output) = child.wait_with_output() else {
        return (false, false);
    };
    let line = String::from_utf8_lossy(&output.stdout);
    let mut parts = line.split_whitespace();
    let app_present = parts.next().is_some_and(|p| p == "True");
    let app_link = parts.next().is_some_and(|p| p == "True");
    (app_present, app_link)
}
