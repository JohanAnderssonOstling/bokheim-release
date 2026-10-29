use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct OpenFiles(HashMap<String, usize>);

impl OpenFiles {
    pub(crate) fn open(&mut self, name: &str) {
        *self.0.entry(name.to_owned()).or_default() += 1;
    }

    pub(crate) fn close(&mut self, name: &str) -> bool {
        let Some(count) = self.0.get_mut(name) else { return false };
        *count -= 1;
        if *count == 0 {
            self.0.remove(name);
        }
        true
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::OpenFiles;

    #[test]
    fn closing_one_reader_keeps_the_file_open_for_other_readers() {
        let mut files = OpenFiles::default();
        files.open("taxonomy.sqlite3");
        files.open("taxonomy.sqlite3");
        assert!(files.close("taxonomy.sqlite3"));
        assert!(!files.is_empty());
        assert!(files.close("taxonomy.sqlite3"));
        assert!(files.is_empty());
        assert!(!files.close("taxonomy.sqlite3"));
    }

    #[test]
    fn independent_files_keep_independent_counts() {
        let mut files = OpenFiles::default();
        files.open("first.sqlite3");
        files.open("second.sqlite3");
        assert!(files.close("first.sqlite3"));
        assert!(!files.is_empty());
        assert!(files.close("second.sqlite3"));
        assert!(files.is_empty());
    }
}
