use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

/// Minimal client for herdr's newline-delimited-JSON socket API.
pub struct HerdrClient {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl HerdrClient {
    pub fn connect() -> io::Result<Self> {
        let path = std::env::var_os("HERDR_SOCKET_PATH")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HERDR_SOCKET_PATH not set"))?;
        Ok(Self::from_stream(UnixStream::connect(path)?))
    }

    pub fn from_stream(stream: UnixStream) -> Self {
        let reader = BufReader::new(stream.try_clone().expect("clone unix stream"));
        Self { writer: stream, reader, next_id: 0 }
    }

    /// NOTE: do not call this on a connection that is also being drained via
    /// `read_line` for subscribed events — pushed events that arrive during
    /// the request's round-trip are skipped and lost.
    pub fn request(&mut self, method: &str, params: Value) -> io::Result<Value> {
        self.next_id += 1;
        let id = format!("req_{}", self.next_id);
        writeln!(self.writer, "{}", json!({"id": id, "method": method, "params": params}))?;
        loop {
            let msg = self.read_line()?;
            if msg.get("id").and_then(Value::as_str) != Some(id.as_str()) {
                continue; // pushed event or stale reply — not ours
            }
            if let Some(err) = msg.get("error") {
                return Err(io::Error::other(err.to_string()));
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// One newline-delimited JSON message; UnexpectedEof when herdr closes
    /// the socket (i.e. the session ended).
    pub fn read_line(&mut self) -> io::Result<Value> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "herdr socket closed"));
        }
        serde_json::from_str(&line).map_err(io::Error::other)
    }

    pub fn send_text(&mut self, pane_id: &str, text: &str) -> io::Result<()> {
        self.request("pane.send_text", json!({"pane_id": pane_id, "text": text}))?;
        Ok(())
    }

    pub fn focused_pane_id(&mut self) -> io::Result<Option<String>> {
        let result = self.request("pane.list", json!({}))?;
        Ok(find_focused_pane(&result))
    }

    pub fn subscribe_pane_focus(&mut self) -> io::Result<()> {
        self.request(
            "events.subscribe",
            json!({"subscriptions": [{"type": "pane.focused"}]}),
        )?;
        Ok(())
    }
}

/// `pane.list` result shape is undocumented; accept `{"panes":[...]}` or a
/// bare array, `focused`/`is_focused` flags, and `id`/`pane_id` keys.
pub fn find_focused_pane(result: &Value) -> Option<String> {
    let panes = result.get("panes").and_then(Value::as_array).or_else(|| result.as_array())?;
    panes
        .iter()
        .find(|p| {
            p.get("focused").and_then(Value::as_bool).unwrap_or(false)
                || p.get("is_focused").and_then(Value::as_bool).unwrap_or(false)
        })
        .and_then(|p| p.get("id").or_else(|| p.get("pane_id")))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Pull a pane id out of a `pane.focused` event, tolerating several shapes.
pub fn event_pane_id(event: &Value) -> Option<String> {
    [
        event.get("pane_id"),
        event.get("pane").and_then(|p| p.get("id")),
        event.get("event").and_then(|e| e.get("pane_id")),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_str)
    .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    #[test]
    fn request_matches_response_id_and_skips_interleaved_events() {
        let (client_side, server_side) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut reader = BufReader::new(server_side.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(req["method"], "pane.send_text");
            assert_eq!(req["params"]["pane_id"], "w1:p1");
            let mut w = &server_side;
            writeln!(w, r#"{{"type":"pane.focused","pane_id":"w1:p9"}}"#).unwrap();
            writeln!(w, r#"{{"id":{},"result":{{"ok":true}}}}"#, req["id"]).unwrap();
        });
        let mut client = HerdrClient::from_stream(client_side);
        let result = client
            .request("pane.send_text", json!({"pane_id": "w1:p1", "text": "hi"}))
            .unwrap();
        assert_eq!(result["ok"], true);
        server.join().unwrap();
    }

    #[test]
    fn error_response_becomes_err() {
        let (client_side, server_side) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut reader = BufReader::new(server_side.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            let mut w = &server_side;
            writeln!(w, r#"{{"id":{},"error":{{"code":"not_found","message":"pane not found"}}}}"#, req["id"]).unwrap();
        });
        let mut client = HerdrClient::from_stream(client_side);
        let err = client.request("pane.get", json!({})).unwrap_err();
        assert!(err.to_string().contains("not_found"));
        server.join().unwrap();
    }

    #[test]
    fn closed_socket_is_unexpected_eof() {
        let (client_side, server_side) = UnixStream::pair().unwrap();
        drop(server_side);
        let mut client = HerdrClient::from_stream(client_side);
        let err = client.read_line().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn find_focused_pane_accepts_known_shapes() {
        let wrapped = json!({"panes": [
            {"id": "w1:p1", "focused": false},
            {"id": "w1:p2", "focused": true},
        ]});
        assert_eq!(find_focused_pane(&wrapped), Some("w1:p2".into()));

        let bare = json!([{"pane_id": "w1:p3", "is_focused": true}]);
        assert_eq!(find_focused_pane(&bare), Some("w1:p3".into()));

        let none = json!({"panes": [{"id": "w1:p1", "focused": false}]});
        assert_eq!(find_focused_pane(&none), None);
    }

    #[test]
    fn event_pane_id_accepts_known_shapes() {
        assert_eq!(event_pane_id(&json!({"pane_id": "a"})), Some("a".into()));
        assert_eq!(event_pane_id(&json!({"pane": {"id": "b"}})), Some("b".into()));
        assert_eq!(event_pane_id(&json!({"event": {"pane_id": "c"}})), Some("c".into()));
        assert_eq!(event_pane_id(&json!({"type": "workspace.created"})), None);
    }
}
