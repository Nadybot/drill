use tokio::sync::mpsc::UnboundedSender;

pub enum StreamEvent {
    New {
        id: String,
        sender: UnboundedSender<StreamEvent>,
    },
    Data {
        id: String,
        data: Vec<u8>,
    },
    Close {
        id: String,
    },
}

impl StreamEvent {
    pub fn new(id: String, sender: UnboundedSender<StreamEvent>) -> Self {
        Self::New { id, sender }
    }

    pub fn data(id: String, data: Vec<u8>) -> Self {
        Self::Data { id, data }
    }

    pub fn close(id: String) -> Self {
        Self::Close { id }
    }
}
