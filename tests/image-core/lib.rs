//! What the viewer believes about a folder of pictures, without a window.
//!
//! The app these rules belong to shipped for months unable to open a file at all, so the rules
//! had never been exercised anywhere except through the shell's own screen.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use yantrik_image_core::{format_file_size, is_image, Gallery};

    static ID: AtomicUsize = AtomicUsize::new(0);

    struct Folder(PathBuf);
    impl Folder {
        fn new(names: &[&str]) -> Self {
            let p = std::env::temp_dir().join(format!(
                "image-core-{}-{}",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            for n in names {
                fs::write(p.join(n), b"not really a picture, and it does not have to be").unwrap();
            }
            Self(p)
        }
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_picture_is_known_by_its_extension_whatever_its_case() {
        assert!(is_image("photo.jpg"));
        assert!(is_image("ICON.PNG"));
        assert!(is_image("art.webp"));
        assert!(is_image("scan.TIFF"));
        assert!(!is_image("notes.txt"));
        assert!(!is_image("archive.tar.gz"));
        assert!(!is_image("README"));
        // A name that only contains the word is not a file of that type.
        assert!(!is_image("png"));
    }

    #[test]
    fn the_folder_holds_only_its_pictures_in_name_order() {
        let f = Folder::new(&["b.png", "a.jpg", "notes.txt", "c.gif", "run.sh"]);
        let g = Gallery::open(&f.path("a.jpg"));
        let names: Vec<String> = g
            .paths()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["a.jpg", "b.png", "c.gif"]);
        assert_eq!(g.index(), 0);
        assert_eq!(g.counter_text(), "1 / 3");
    }

    #[test]
    fn opening_one_picture_selects_that_picture_not_the_first() {
        let f = Folder::new(&["a.jpg", "b.png", "c.gif"]);
        let g = Gallery::open(&f.path("b.png"));
        assert_eq!(g.current().unwrap().file_name().unwrap(), "b.png");
        assert_eq!(g.counter_text(), "2 / 3");
    }

    #[test]
    fn next_and_previous_wrap_around_the_folder() {
        let f = Folder::new(&["a.jpg", "b.png", "c.gif"]);
        let mut g = Gallery::open(&f.path("c.gif"));
        assert_eq!(g.next().unwrap().file_name().unwrap(), "a.jpg", "past the end is the start");
        assert_eq!(g.prev().unwrap().file_name().unwrap(), "c.gif", "and back again");
        assert_eq!(g.prev().unwrap().file_name().unwrap(), "b.png");
    }

    #[test]
    fn a_folder_with_one_picture_stays_on_it() {
        let f = Folder::new(&["only.png"]);
        let mut g = Gallery::open(&f.path("only.png"));
        g.next();
        g.next();
        assert_eq!(g.current().unwrap().file_name().unwrap(), "only.png");
        assert_eq!(g.counter_text(), "1 / 1");
    }

    #[test]
    fn a_file_that_is_not_there_is_still_the_file_that_was_asked_for() {
        // The viewer has to name what it could not open. An empty gallery would leave the
        // window blank with nothing to say.
        let f = Folder::new(&["a.jpg"]);
        let mut g = Gallery::open(&f.path("gone.png"));
        assert_eq!(
            g.current().unwrap().file_name().unwrap(),
            "gone.png",
            "the viewer shows the file it was asked for, so it can say it cannot open it"
        );
        // And the pictures that are there are still reachable from it.
        assert_eq!(g.next().unwrap().file_name().unwrap(), "a.jpg");
    }

    #[test]
    fn selecting_a_picture_already_open_keeps_the_folder() {
        let f = Folder::new(&["a.jpg", "b.png"]);
        let mut g = Gallery::open(&f.path("a.jpg"));
        assert!(g.select(&f.path("b.png")));
        assert_eq!(g.index(), 1);
        assert_eq!(g.len(), 2, "still the same folder, not reopened around one file");
        assert!(!g.select(&f.path("elsewhere.png")));
        assert_eq!(g.index(), 1, "a file it does not hold leaves the selection alone");
    }

    #[test]
    fn nothing_open_is_a_gallery_with_nothing_in_it() {
        let mut g = Gallery::empty();
        assert!(g.is_empty());
        assert_eq!(g.counter_text(), "");
        assert!(g.current().is_none());
        assert!(g.next().is_none());
        assert!(g.prev().is_none());
    }

    #[test]
    fn file_sizes_read_the_way_a_person_says_them() {
        assert_eq!(format_file_size(512), "512 B");
        assert_eq!(format_file_size(2048), "2.0 KB");
        assert_eq!(format_file_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn a_file_with_no_exif_still_reports_what_the_filesystem_knows() {
        let f = Folder::new(&["plain.png"]);
        let info = yantrik_image_core::read_info(&f.path("plain.png"));
        assert_eq!(info.format, "PNG");
        assert!(!info.file_size.is_empty());
        // No camera invented for a file that never had one.
        assert_eq!(info.camera, "");
        assert_eq!(info.date_taken, "");
    }
}
