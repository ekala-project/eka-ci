use anyhow::Context;
use shared::types as t;
use shared::types::{ClientRequest, ClientResponse};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use tracing::debug;

pub fn send_request(socket: &std::path::Path, request: ClientRequest) -> anyhow::Result<()> {
    // attempt to connect to socket
    debug!("Attempting to connect to {}", &socket.display());

    let mut stream = UnixStream::connect(socket).context("failed to connect to server socket")?;

    // send request
    let request_message =
        serde_json::to_string(&request).expect("Our types should always be serializable");
    stream
        .write_all(request_message.as_bytes())
        .context("failed to write request data")?;
    stream.flush().context("failed to flush request data")?;
    // TODO: Figure out why the write side of the stream
    // needs to be shutdown in order to read without
    // blocking both streams
    stream
        .shutdown(Shutdown::Write)
        .context("failed to shutdown connection with server socket")?;

    debug!("Attempting to read response message");
    let mut response_message = String::new();
    stream
        .read_to_string(&mut response_message)
        .context("failed to read server response")?;

    let response: ClientResponse =
        serde_json::from_str(&response_message).context("failed to interpret server response")?;

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
        r::Build(info) => {
            println!("DrvID: {}", &info.drv_id);
        }
    }
}

fn print_info(info: t::InfoResponse) {
    println!("Server status: {:?}", &info.status);
    println!("EkaCI server version: {:?}", &info.version);
}
