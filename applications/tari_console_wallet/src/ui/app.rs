// Copyright 2020. The Tari Project
//
// Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
// following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
// disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
// following disclaimer in the documentation and/or other materials provided with the distribution.
//
// 3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
// products derived from this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
// INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
// WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
// USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use crate::ui::{
    components::{
        base_node::BaseNode,
        network_tab::NetworkTab,
        send_receive_tab::SendReceiveTab,
        tabs_container::TabsContainer,
        transactions_tab::TransactionsTab,
        Component,
    },
    state::AppState,
    MAX_WIDTH,
};
use log::*;
use tari_common::Network;
use tari_comms::{
    multiaddr::Multiaddr,
    peer_manager::{NodeId, Peer, PeerFeatures, PeerFlags},
};
use tari_core::transactions::types::PublicKey;
use tari_crypto::tari_utilities::hex::Hex;
use tari_wallet::WalletSqlite;
use tokio::runtime::Handle;
use tui::{
    backend::Backend,
    layout::{Constraint, Direction, Layout},
    Frame,
};

pub const LOG_TARGET: &str = "wallet::ui::app";
pub const CUSTOM_BASE_NODE_PUBLIC_KEY_KEY: &str = "console_wallet_custom_base_node_public_key";
pub const CUSTOM_BASE_NODE_ADDRESS_KEY: &str = "console_wallet_custom_base_node_address";

pub struct App<B: Backend> {
    pub title: String,
    pub should_quit: bool,
    // Cached state this will need to be cleaned up into a threadsafe container
    pub app_state: AppState,
    // Ui working state
    pub tabs: TabsContainer<B>,
    pub base_node_status: BaseNode,
}

impl<B: Backend> App<B> {
    pub async fn new(title: String, mut wallet: WalletSqlite, network: Network, base_node_config: Peer) -> Self {
        // Attempt to read a stored custom base node public key and address from the wallet database. If this fails we
        // will not use a custom peer and fall back to the config peer
        let custom_peer = get_custom_base_node_peer_from_db(&mut wallet).await;

        let app_state = AppState::new(
            wallet.comms.node_identity().as_ref(),
            network,
            wallet,
            base_node_config.clone(),
            custom_peer.clone(),
        );

        // If there is a custom peer we initialize the Network tab with it, otherwise we use the peer provided from
        // config
        let (public_key_str, public_address_str) = if let Some(custom_peer) = custom_peer {
            let public_address = match custom_peer.addresses.first() {
                Some(address) => address.to_string(),
                None => "".to_string(),
            };
            info!(
                target: LOG_TARGET,
                "Using stored custom base node - {}::{}", custom_peer.public_key, public_address
            );
            (custom_peer.public_key.to_hex(), public_address)
        } else {
            let public_address = match base_node_config.addresses.first() {
                Some(address) => address.to_string(),
                None => "".to_string(),
            };
            info!(
                target: LOG_TARGET,
                "Using configuration specified base node - {}::{}", base_node_config.public_key, public_address
            );
            (base_node_config.public_key.to_hex(), public_address)
        };

        let tabs = TabsContainer::<B>::new(title.clone())
            .add("Transactions".into(), Box::new(TransactionsTab::new()))
            .add("Send/Receive".into(), Box::new(SendReceiveTab::new()))
            .add(
                "Network".into(),
                Box::new(NetworkTab::new(public_key_str, public_address_str)),
            );

        let base_node_status = BaseNode::new();

        Self {
            title,
            should_quit: false,
            app_state,
            tabs,
            base_node_status,
        }
    }

    pub fn on_control_key(&mut self, c: char) {
        match c {
            'q' | 'c' => {
                self.should_quit = true;
            },
            _ => (),
        }
    }

    pub fn on_key(&mut self, c: char) {
        match c {
            '\t' => {
                self.tabs.next();
            },
            _ => self.tabs.on_key(&mut self.app_state, c),
        }
    }

    pub fn on_up(&mut self) {
        self.tabs.on_up(&mut self.app_state);
    }

    pub fn on_down(&mut self) {
        self.tabs.on_down(&mut self.app_state);
    }

    pub fn on_right(&mut self) {
        self.tabs.next();
    }

    pub fn on_left(&mut self) {
        self.tabs.previous();
    }

    pub fn on_esc(&mut self) {
        self.tabs.on_esc(&mut self.app_state);
    }

    pub fn on_backspace(&mut self) {
        self.tabs.on_backspace(&mut self.app_state);
    }

    pub fn on_tick(&mut self) {
        Handle::current().block_on(self.app_state.update_cache());
        self.tabs.on_tick(&mut self.app_state);
    }

    pub fn draw(&mut self, f: &mut Frame<'_, B>) {
        let max_width_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(MAX_WIDTH), Constraint::Min(0)].as_ref())
            .split(f.size());
        let title_chunks = Layout::default()
            .constraints([Constraint::Length(3), Constraint::Min(0)].as_ref())
            .split(max_width_layout[0]);
        let title_halves = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
            .split(title_chunks[0]);

        self.tabs.draw_titles(f, title_halves[0]);

        self.base_node_status.draw(f, title_halves[1], &self.app_state);
        self.tabs.draw_content(f, title_chunks[1], &mut self.app_state);
    }
}

/// This helper function will attempt to read a stored base node public key and address from the wallet database if
/// possible. If both are found they are used to construct and return a Peer.
async fn get_custom_base_node_peer_from_db(wallet: &mut WalletSqlite) -> Option<Peer> {
    let custom_base_node_peer_pubkey = match wallet
        .db
        .get_client_key_value(CUSTOM_BASE_NODE_PUBLIC_KEY_KEY.to_string())
        .await
    {
        Ok(val) => val,
        Err(e) => {
            warn!(target: LOG_TARGET, "Problem reading from wallet database: {}", e);
            return None;
        },
    };
    let custom_base_node_peer_address = match wallet
        .db
        .get_client_key_value(CUSTOM_BASE_NODE_ADDRESS_KEY.to_string())
        .await
    {
        Ok(val) => val,
        Err(e) => {
            warn!(target: LOG_TARGET, "Problem reading from wallet database: {}", e);
            return None;
        },
    };

    match (custom_base_node_peer_pubkey, custom_base_node_peer_address) {
        (Some(public_key), Some(address)) => {
            let pub_key_str = PublicKey::from_hex(public_key.as_str());
            let addr_str = address.parse::<Multiaddr>();
            let (pub_key, address) = match (pub_key_str, addr_str) {
                (Ok(pk), Ok(addr)) => (pk, addr),
                (_, _) => {
                    debug!(
                        target: LOG_TARGET,
                        "Problem converting stored custom base node public key or address"
                    );
                    return None;
                },
            };

            let node_id = match NodeId::from_key(&pub_key) {
                Ok(n) => n,
                Err(e) => {
                    debug!(
                        target: LOG_TARGET,
                        "Problem converting stored base node public key to Node Id: {}", e
                    );
                    return None;
                },
            };
            Some(Peer::new(
                pub_key,
                node_id,
                address.into(),
                PeerFlags::default(),
                PeerFeatures::COMMUNICATION_NODE,
                &[],
                Default::default(),
            ))
        },
        (_, _) => None,
    }
}
