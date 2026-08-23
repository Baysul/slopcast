pub fn with_command_line_args(builder: tauri::Builder<tauri::Cef>) -> tauri::Builder<tauri::Cef> {
    builder.command_line_args([(
        "--remote-debugging-port".to_string(),
        Some("9222".to_string()),
    )])
}
