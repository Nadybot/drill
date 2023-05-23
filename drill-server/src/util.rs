use std::iter::repeat_with;

use uuid::Uuid;

pub fn random_token() -> String {
    Uuid::new_v4().to_string()
}

pub fn random_subdomain() -> String {
    repeat_with(fastrand::alphanumeric).take(10).collect()
}
