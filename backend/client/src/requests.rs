use crate::cli;
use crate::error::Result;
use shared::dirs::eka_dirs;
use shared::types as t;
use shared::types::{ClientRequest, ClientResponse};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use tracing::debug;

// TODO: Better error handling
pub fn send_request(args: cli::Args, request: ClientRequest) -> Result<()> {
    // attempt to connect to socket
    let socket = args.socket.unwrap_or_else(|| {
        eka_dirs()
            .get_runtime_file("ekaci.socket")
            .expect("failed to find xdg_runtime_dir after socket not set")
            .to_str()
            .expect("failed to make socket path into string")
            .to_string()
    });

    debug!("Attempting to connect to {}", &socket);

    let mut stream = UnixStream::connect(socket)?;

    // send request
    let request_message = serde_json::to_string(&request)?;
    stream.write_all(request_message.as_bytes())?;
    stream.flush()?;
    // TODO: Figure out why the write side of the stream
    // needs to be shutdown in order to read without
    // blocking both streams
    stream.shutdown(Shutdown::Write)?;

    debug!("Attempting to write response message");
    let mut response_message = String::new();
    stream.read_to_string(&mut response_message)?;

    let response: ClientResponse = serde_json::from_str(&response_message)?;

    // render response
    handle_response(response);

    Ok(())
}

fn handle_response(response: ClientResponse) {
    use shared::types::ClientResponse as r;

    match response {
        r::Info(info) => {
            print_info(info);
        }
    }
}

fn print_info(info: t::InfoResponse) {
    println!("Server status: {:?}", &info.status);
    println!("EkaCI server version: {:?}", &info.version);
}
