//! Asks the router (UPnP) to forward the server's UDP port, so players outside the network can
//! join without setting up a port forward by hand. Renewed every half hour, in the background.

use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::time::Duration;

use crate::say;

use igd_next::{PortMappingProtocol, SearchOptions, search_gateway};

const LEASE_SECONDS: u32 = 3600;

/// This machine's address on the network the gateway is on.
fn local_address(gateway: SocketAddr, port: u16) -> Option<SocketAddrV4> {
    let probe = UdpSocket::bind("0.0.0.0:0").ok()?;
    probe.connect(gateway).ok()?;
    match probe.local_addr().ok()? {
        SocketAddr::V4(v4) => Some(SocketAddrV4::new(*v4.ip(), port)),
        SocketAddr::V6(_) => None,
    }
}

pub fn open(port: u16) {
    std::thread::spawn(move || {
        let mut first = true;
        loop {
            match search_gateway(SearchOptions { timeout: Some(Duration::from_secs(5)), ..Default::default() }) {
                Ok(gateway) => match local_address(gateway.addr, port) {
                    Some(local) => match gateway.add_port(PortMappingProtocol::UDP, port, SocketAddr::V4(local), LEASE_SECONDS, "IVMP server") {
                        Ok(()) if first => {
                            match gateway.get_external_ip() {
                                Ok(ip) => say!("upnp: the router forwards UDP {port} to {local}; players join {ip}:{port}"),
                                Err(_) => say!("upnp: the router forwards UDP {port} to {local}"),
                            }
                            first = false;
                        }
                        Ok(()) => {}
                        Err(e) if first => {
                            say!("upnp: the router refused the port forward: {e} (forward UDP {port} to {local} by hand)");
                            first = false;
                        }
                        Err(_) => {}
                    },
                    None if first => {
                        say!("upnp: could not find this machine's address");
                        first = false;
                    }
                    None => {}
                },
                Err(e) if first => {
                    say!("upnp: no router answered ({e}); is UPnP turned on in the router?");
                    first = false;
                }
                Err(_) => {}
            }
            std::thread::sleep(Duration::from_secs(1800));
        }
    });
}
