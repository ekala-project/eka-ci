use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use anyhow::Context;
use shared::types as t;
use shared::types::{ClientRequest, ClientResponse};
use tracing::debug;

pub fn send_request(socket: &std::path::Path, request: ClientRequest) -> anyhow::Result<()> {
    // attempt to connect to socket
    debug!("Attempting to connect to {}", &socket.display());

    let mut stream = UnixStream::connect(socket).context("failed to connect to server socket")?;

    // send request using newline-delimited JSON protocol
    // The newline acts as a message boundary, allowing the server to know when
    // the message is complete without requiring EOF (shutdown)
    let mut request_message =
        serde_json::to_string(&request).expect("Our types should always be serializable");
    request_message.push('\n');
    stream
        .write_all(request_message.as_bytes())
        .context("failed to write request data")?;
    stream.flush().context("failed to flush request data")?;

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
        },
        r::Ack(was_successful) => {
            println!("Queued Successfully: {}", &was_successful);
        },
        r::DrvStatus(info) => {
            print_drv_status(info);
        },
    }
}

fn print_info(info: t::InfoResponse) {
    println!("Server status: {:?}", &info.status);
    println!("EkaCI server version: {:?}", &info.version);
}

fn print_drv_status(result: Result<t::DrvStatusResponse, String>) {
    match result {
        Err(error) => {
            eprintln!("Error: {}", error);
        },
        Ok(drv) => {
            println!("Drv: {:?}", &drv.drv_path);
            println!("Status: {:?}", drv.status);

            // Display failed dependencies if present
            if let Some(failed_deps) = drv.failed_dependencies {
                println!("\nFailed dependencies ({}):", failed_deps.len());
                for dep in failed_deps {
                    println!("  - {}", dep);
                }
            }
        },
    }
}
