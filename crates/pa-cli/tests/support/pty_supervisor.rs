pub(super) fn serve(
    listener: &std::os::unix::net::UnixListener,
    serve_connection: fn(std::os::unix::net::UnixStream),
) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => serve_connection(stream),
            Err(_) => return,
        }
    }
}
