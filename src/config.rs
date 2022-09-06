/// Configuration for the readline prompt
pub struct RlwrapConfig {
    /// The prefix of the prompt. E.g. "cool app> ".
    pub prefix: String,
    /// The prompt thread will send a interrupt signal on CTRL+C.
    /// If this is enabled it will also stop the prompt.
    /// You may set this to false if you want to handle interrupt signals.
    pub stop_on_ctrl_c: bool,
}

impl Default for RlwrapConfig {
    fn default() -> Self {
        Self {
            prefix: "> ".to_owned(),
            stop_on_ctrl_c: true,
        }
    }
}
