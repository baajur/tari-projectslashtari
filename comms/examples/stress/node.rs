//  Copyright 2020, The Tari Project
//
//  Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
//  following conditions are met:
//
//  1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
//  disclaimer.
//
//  2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
//  following disclaimer in the documentation and/or other materials provided with the distribution.
//
//  3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
//  products derived from this software without specific prior written permission.
//
//  THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
//  INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
//  DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
//  SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
//  SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
//  WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
//  USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use super::{error::Error, STRESS_PROTOCOL_NAME, TOR_CONTROL_PORT_ADDR, TOR_SOCKS_ADDR};
use futures::channel::mpsc;
use rand::rngs::OsRng;
use std::{net::Ipv4Addr, path::Path, sync::Arc};
use tari_comms::{
    multiaddr::Multiaddr,
    protocol::{ProtocolNotification, Protocols},
    tor,
    tor::TorIdentity,
    transports::{SocksConfig, TcpWithTorTransport},
    CommsBuilder,
    CommsNode,
    NodeIdentity,
    Substream,
};
use tari_storage::{lmdb_store::LMDBBuilder, LMDBWrapper};

pub async fn create(
    node_identity: Option<Arc<NodeIdentity>>,
    database_path: &Path,
    public_ip: Option<Ipv4Addr>,
    port: u16,
    tor_identity: Option<TorIdentity>,
    is_tcp: bool,
) -> Result<(CommsNode, mpsc::Receiver<ProtocolNotification<Substream>>), Error>
{
    let datastore = LMDBBuilder::new()
        .set_path(database_path.to_str().unwrap())
        .set_environment_size(50)
        .set_max_number_of_databases(1)
        .add_database("peerdb", lmdb_zero::db::CREATE)
        .build()
        .unwrap();
    let peer_database = datastore.get_handle(&"peerdb").unwrap();
    let peer_database = LMDBWrapper::new(Arc::new(peer_database));

    let mut protocols = Protocols::new();
    let (proto_notif_tx, proto_notif_rx) = mpsc::channel(1);
    protocols.add(&[STRESS_PROTOCOL_NAME.clone()], proto_notif_tx);

    let public_addr = format!(
        "/ip4/{}/tcp/{}",
        public_ip
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "0.0.0.0".to_string()),
        port
    )
    .parse::<Multiaddr>()
    .unwrap();
    let node_identity = node_identity
        .map(|ni| {
            ni.set_public_address(public_addr.clone());
            ni
        })
        .unwrap_or_else(|| Arc::new(NodeIdentity::random(&mut OsRng, public_addr, Default::default()).unwrap()));

    let listener_addr = format!("/ip4/0.0.0.0/tcp/{}", port).parse().unwrap();

    let builder = CommsBuilder::new()
        .allow_test_addresses()
        .with_protocols(protocols)
        .with_node_identity(node_identity.clone())
        .with_peer_storage(peer_database)
        .disable_connection_reaping();

    let comms_node = if is_tcp {
        builder
            .with_listener_address(listener_addr)
            .with_transport(TcpWithTorTransport::with_tor_socks_proxy(SocksConfig {
                proxy_address: TOR_SOCKS_ADDR.parse().unwrap(),
                authentication: Default::default(),
            }))
            .build()?
            .spawn()
            .await?
    } else {
        let mut hs_builder = tor::HiddenServiceBuilder::new()
            .with_port_mapping(port)
            .with_control_server_address(TOR_CONTROL_PORT_ADDR.parse().unwrap());

        if let Some(tor_identity) = tor_identity {
            hs_builder = hs_builder.with_tor_identity(tor_identity);
        }

        let tor_hidden_service = hs_builder.finish().await?;

        println!(
            "Tor hidden service created with address '{}'",
            tor_hidden_service.get_onion_address()
        );

        node_identity.set_public_address(tor_hidden_service.get_onion_address());
        builder
            .configure_from_hidden_service(tor_hidden_service)
            .build()?
            .spawn()
            .await?
    };

    Ok((comms_node, proto_notif_rx))
}
