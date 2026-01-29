#[derive(Debug, Clone)]
pub struct Session {
    pub name: String,
    #[allow(dead_code)]
    pub activity: i64,
    pub attached: bool,
    pub pane_pid: u32,
}

impl Session {
    pub fn new(name: String, activity: i64, attached: bool, pane_pid: u32) -> Self {
        Self {
            name,
            activity,
            attached,
            pane_pid,
        }
    }
}
