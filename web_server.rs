use std::{
    env,
    fs::File,
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Component, Path, PathBuf},
    thread,
};

const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:80";
const MAX_REQUEST_BYTES: usize = 8192;

fn main() -> io::Result<()> {
    let root = env::current_dir()?.canonicalize()?;
    let listen_addr = listen_addr();
    let listener = TcpListener::bind(&listen_addr)?;
    println!("Serving {} on http://{}", root.display(), listen_addr);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let root = root.clone();
                thread::spawn(move || {
                    let _ = handle_connection(stream, &root);
                });
            }
            Err(error) => eprintln!("accept failed: {error}"),
        }
    }

    Ok(())
}

fn listen_addr() -> String {
    let mut args = env::args().skip(1);
    match args.next() {
        Some(arg) if arg == "--port" => {
            let port = args.next().unwrap_or_else(|| "80".to_string());
            format!("0.0.0.0:{port}")
        }
        Some(arg) if arg.starts_with("--port=") => {
            format!("0.0.0.0:{}", arg.trim_start_matches("--port="))
        }
        Some(arg) if arg.contains(':') => arg,
        Some(arg) => format!("0.0.0.0:{arg}"),
        None => DEFAULT_LISTEN_ADDR.to_string(),
    }
}

fn handle_connection(mut stream: TcpStream, root: &Path) -> io::Result<()> {
    let request = read_request_head(&mut stream)?;
    let Some((method, target)) = parse_request_line(&request) else {
        return write_error(&mut stream, 400, "Bad Request");
    };

    if method != "GET" && method != "HEAD" {
        return write_error(&mut stream, 405, "Method Not Allowed");
    }

    if target == "/favicon.ico" {
        return write_response(&mut stream, method == "HEAD", "image/x-icon", None, &[]);
    }

    let Some(path) = safe_request_path(root, target) else {
        return write_error(&mut stream, 404, "Not Found");
    };

    let Some(mime_type) = mime_type_for(&path) else {
        return write_error(&mut stream, 404, "Not Found");
    };

    let br_path = with_br_extension(&path);
    if let Some(body) = read_safe_file(root, &br_path) {
        return write_response(&mut stream, method == "HEAD", mime_type, Some("br"), &body);
    }

    if let Some(body) = read_safe_file(root, &path) {
        return write_response(&mut stream, method == "HEAD", mime_type, None, &body);
    }

    write_error(&mut stream, 404, "Not Found")
}

fn read_request_head(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut request = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];

    while request.len() < MAX_REQUEST_BYTES {
        let len = stream.read(&mut byte)?;
        if len == 0 {
            break;
        }
        request.push(byte[0]);
        if request.ends_with(b"\r\n\r\n") || request.ends_with(b"\n\n") {
            break;
        }
    }

    Ok(request)
}

fn parse_request_line(request: &[u8]) -> Option<(&str, &str)> {
    let request = std::str::from_utf8(request).ok()?;
    let line = request.lines().next()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    let version = parts.next()?;

    if !version.starts_with("HTTP/") || parts.next().is_some() {
        return None;
    }

    Some((method, target))
}

fn safe_request_path(root: &Path, target: &str) -> Option<PathBuf> {
    let target = target.split_once('?').map_or(target, |(path, _)| path);
    let target = target.split_once('#').map_or(target, |(path, _)| path);
    if !target.starts_with('/') {
        return None;
    }

    let mut decoded = percent_decode(target)?;
    if decoded == "/" || decoded.ends_with('/') {
        decoded.push_str("index.html");
    }

    if decoded.as_bytes().contains(&0) || decoded.contains('\\') {
        return None;
    }

    let relative = decoded.trim_start_matches('/');
    let relative_path = Path::new(relative);
    if relative_path.is_absolute() {
        return None;
    }

    let mut clean = PathBuf::new();
    for component in relative_path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            _ => return None,
        }
    }

    let joined = root.join(clean);
    ensure_under_root(root, &joined).then_some(joined)
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let high = hex_value(bytes[index + 1])?;
            let low = hex_value(bytes[index + 2])?;
            output.push((high << 4) | low);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }

    String::from_utf8(output).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn read_safe_file(root: &Path, path: &Path) -> Option<Vec<u8>> {
    if !ensure_under_root(root, path) {
        return None;
    }

    let canonical = path.canonicalize().ok()?;
    if !canonical.starts_with(root) || !canonical.is_file() {
        return None;
    }

    let mut file = File::open(canonical).ok()?;
    let mut body = Vec::new();
    file.read_to_end(&mut body).ok()?;
    Some(body)
}

fn with_br_extension(path: &Path) -> PathBuf {
    let mut name = path.file_name().and_then(|name| name.to_str()).unwrap_or("").to_string();
    name.push_str(".br");
    path.with_file_name(name)
}

fn ensure_under_root(root: &Path, path: &Path) -> bool {
    if path.is_absolute() {
        path.starts_with(root)
    } else {
        false
    }
}

fn write_response(
    stream: &mut TcpStream,
    head_only: bool,
    mime_type: &str,
    encoding: Option<&str>,
    body: &[u8],
) -> io::Result<()> {
    let mut header = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: {mime_type}\r\n\
         Cross-Origin-Embedder-Policy: require-corp\r\n\
         Cross-Origin-Opener-Policy: same-origin\r\n\
         Cache-Control: max-age=0\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n",
        body.len()
    );

    if let Some(encoding) = encoding {
        header.push_str(&format!("Content-Encoding: {encoding}\r\n"));
    }
    header.push_str("\r\n");

    stream.write_all(header.as_bytes())?;
    if !head_only {
        stream.write_all(body)?;
    }
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}

fn write_error(stream: &mut TcpStream, status: u16, reason: &str) -> io::Result<()> {
    let body = format!("{status} {reason}\n");
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/plain\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}

fn mime_type_for(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()? {
        "html" => Some("text/html"),
        "wasm" => Some("application/wasm"),
        "css" => Some("text/css"),
        "js" => Some("text/javascript"),
        "ttf" => Some("application/ttf"),
        "otf" => Some("font/otf"),
        "png" => Some("image/png"),
        "jpg" => Some("image/jpg"),
        "jpeg" => Some("image/jpeg"),
        "svg" => Some("image/svg+xml"),
        "md" => Some("text/markdown"),
        "bin" => Some("application/octet-stream"),
        "woff" => Some("font/woff"),
        "woff2" => Some("font/woff2"),
        _ => None,
    }
}
