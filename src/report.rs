use anyhow::Result;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Fail,
    Skip,
}

pub struct Entry {
    pub status: Status,
    pub name: String,
    pub detail: String,
}

#[derive(Default)]
pub struct Report {
    entries: Vec<Entry>,
}

impl Report {
    pub fn check<T>(
        &mut self,
        name: &str,
        result: Result<T>,
        detail: impl Fn(&T) -> String,
    ) -> Option<T> {
        let (status, detail, value) = match result {
            Ok(v) => (Status::Ok, detail(&v), Some(v)),
            Err(e) => (Status::Fail, format!("{e:#}"), None),
        };
        self.entries.push(Entry {
            status,
            name: name.into(),
            detail,
        });
        value
    }

    pub fn failed(&self) -> bool {
        self.entries.iter().any(|e| e.status == Status::Fail)
    }

    pub fn print(&self, only_failures: bool) {
        for e in &self.entries {
            if only_failures && e.status != Status::Fail {
                continue;
            }
            let tag = match e.status {
                Status::Ok => "ok",
                Status::Fail => "FAIL",
                Status::Skip => "skip",
            };
            println!("{tag:<5} {:<28} {}", e.name, e.detail);
        }
    }

    pub fn push(&mut self, e: Entry) {
        self.entries.push(e);
    }

    pub fn skip(&mut self, name: &str, reason: &str) {
        self.entries.push(Entry {
            status: Status::Skip,
            name: name.into(),
            detail: reason.into(),
        });
    }
}
