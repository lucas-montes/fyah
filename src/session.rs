use crate::config::Config;

use super::supervisor::Supervisor;

pub struct Session {
    supervisor: Supervisor,
    config: Config,
}

impl Session {
    pub fn new(supervisor: Supervisor, config: Config) -> Self {
        Self { supervisor, config }
    }
}
