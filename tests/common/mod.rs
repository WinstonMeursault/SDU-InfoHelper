use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

pub struct Request {
    pub headers: String,
    pub body: Vec<u8>,
}

pub fn server(
    status: &str,
    extra_headers: &str,
    body: &str,
    delay: Duration,
) -> (String, mpsc::Receiver<Request>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra_headers}Connection: close\r\n\r\n{body}",
        body.len()
    );
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "HTTP request did not arrive");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        };
        // Windows accepted sockets inherit the listener's nonblocking mode.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut buffer = [0u8; 1024];
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(offset) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                break offset + 4;
            }
        };
        let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
        let length = headers
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        while bytes.len() < header_end + length {
            let mut buffer = [0u8; 1024];
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
        }
        sender
            .send(Request {
                headers,
                body: bytes[header_end..header_end + length].to_vec(),
            })
            .unwrap();
        thread::sleep(delay);
        // The timeout test deliberately closes the client before this write.
        let _ = stream.write_all(response.as_bytes());
    });
    (url, receiver, handle)
}
