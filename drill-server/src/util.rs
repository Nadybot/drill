use uuid::Uuid;

pub fn random_token() -> String {
    Uuid::new_v4().to_string()
}
