use crate::client::SyncClient;
use mpa_agent::{Ticket, TicketError, TicketSource};
use std::sync::{Arc, Mutex};

/// `TicketSource` backed by `GET /api/agent/open-tickets`. The client is
/// behind a mutex so the source stays `Sync` for the agent core.
pub struct HttpTickets {
    client: Mutex<SyncClient>,
}

impl std::fmt::Debug for HttpTickets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpTickets").finish_non_exhaustive()
    }
}

impl HttpTickets {
    pub fn new(client: SyncClient) -> Arc<Self> {
        Arc::new(Self {
            client: Mutex::new(client),
        })
    }
}

impl TicketSource for HttpTickets {
    fn list_open_tickets(&self) -> Result<Vec<Ticket>, TicketError> {
        let client = self.client.lock().expect("sync client");
        client
            .get_open_tickets()
            .map_err(|e| TicketError::Unavailable(e.to_string()))
    }
}
