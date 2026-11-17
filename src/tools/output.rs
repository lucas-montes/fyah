use std::fmt::Write;

/// What a tool that ran to the end said. Exiting non-zero is still an
/// answer: the model is told the status and everything printed on the way.
#[derive(Debug)]
pub struct ToolOutput {
    status: i32,
    stdout: String,
    stderr: String,
}

impl ToolOutput {
    /// Decoded lossily: binary output degrades instead of throwing away
    /// everything the tool said. Valid UTF-8 keeps its buffer.
    pub fn from_bytes(status: i32, stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
        Self {
            status,
            stdout: into_text(stdout),
            stderr: into_text(stderr),
        }
    }

    /// What the model reads. On success only stdout, so warnings on stderr
    /// cannot corrupt an answer such as JSON. On failure stdout, then
    /// stderr, then the status: tools often print something useful first.
    pub fn into_answer(self) -> String {
        if self.status == 0 {
            return self.stdout;
        }
        let mut answer = self.stdout;
        for part in [self.stderr.as_str(), ""] {
            if !answer.is_empty() && !answer.ends_with('\n') {
                answer.push('\n');
            }
            answer.push_str(part);
        }
        // Writing to a `String` cannot fail.
        let _ = write!(answer, "exit status: {}", self.status);
        answer
    }
}

fn into_text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}
