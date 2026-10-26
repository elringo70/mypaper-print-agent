use thiserror::Error;

#[derive(Debug, Error)]
pub enum TicketError {
    #[error("ticket source unavailable: {0}")]
    Unavailable(String),
}

/// Open POS ticket the analysis can be attached to (Phase 4: live MyPaper data).
#[derive(Debug, Clone)]
pub struct Ticket {
    pub id: String,
    pub ticket_number: i32,
    pub cashier: String,
    /// Customer name when the sale has one; most counter sales do not.
    pub customer: Option<String>,
    pub total: f64,
}

pub trait TicketSource: Send + Sync {
    fn list_open_tickets(&self) -> Result<Vec<Ticket>, TicketError>;
}

/// Demo tickets until Phase 4 wires the MyPaper HTTP sync.
#[derive(Debug)]
pub struct MockTickets {
    tickets: Vec<Ticket>,
}

impl MockTickets {
    pub fn demo() -> Self {
        Self {
            tickets: vec![
                Ticket {
                    id: "order-101".into(),
                    ticket_number: 101,
                    cashier: "Ana".into(),
                    customer: Some("María López".into()),
                    total: 85.50,
                },
                Ticket {
                    id: "order-102".into(),
                    ticket_number: 102,
                    cashier: "Luis".into(),
                    customer: None,
                    total: 12.00,
                },
            ],
        }
    }
}

impl TicketSource for MockTickets {
    fn list_open_tickets(&self) -> Result<Vec<Ticket>, TicketError> {
        Ok(self.tickets.clone())
    }
}
