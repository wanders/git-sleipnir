use std::io::Write;

pub struct PktLine {
    data: Vec<u8>,
}

impl PktLine {
    pub fn new() -> PktLine {
        PktLine { data: vec![] }
    }

    pub fn add(mut self, data: &[u8]) -> Self {
        write!(self.data, "{:04x}", data.len() + 4).unwrap();
        self.data.extend_from_slice(data);
        self
    }

    pub fn flush(mut self) -> Self {
        self.data.extend_from_slice(b"0000");
        self
    }
    pub fn delimit(mut self) -> Self {
        self.data.extend_from_slice(b"0001");
        self
    }
    pub fn take(self) -> Vec<u8> {
        self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test() {
        assert_eq!(
            PktLine::new().add(b"foo").add(b"bar").take(),
            b"0007foo0007bar"
        );

        assert_eq!(
            PktLine::new()
                .add(b"x")
                .delimit()
                .add(b"abcd")
                .flush()
                .take(),
            b"0005x00010008abcd0000"
        );
    }
}
