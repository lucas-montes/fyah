use serde_json::Value;

/// Splits a growing SSE byte buffer into completed `data:` JSON events.
///
/// Events are separated by a blank line. Only `data:` lines are parsed; `event:`
/// lines and the `[DONE]` sentinel are ignored. Incomplete trailing data stays in
/// `buf` for the next call.
pub fn split_events(buf: &mut Vec<u8>) -> Vec<Value> {
    let mut events = Vec::new();
    while let Some(pos) = buf.windows(2).position(|w| w == b"\n\n") {
        let chunk = buf[..pos].to_vec(); //TODO: avoid allocation
        buf.drain(..pos + 2);
        let text = String::from_utf8_lossy(&chunk);
        for line in text.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim_start();
                if data == "[DONE]" {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(data) {
                    events.push(value);
                }
            }
        }
    }
    events
}
