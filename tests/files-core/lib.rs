#[path="../../crates/yantrik-ui/src/fileops.rs"]
pub mod fileops;
#[path="../../crates/yantrik-ui/src/filebrowser.rs"]
pub mod filebrowser;
#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs,path::PathBuf,sync::atomic::{AtomicBool,AtomicUsize,Ordering}};
    static ID:AtomicUsize=AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {fn new()->Self {let p=std::env::temp_dir().join(format!("files-test-{}-{}",std::process::id(),ID.fetch_add(1,Ordering::Relaxed)));fs::create_dir(&p).unwrap();Self(p)}}
    impl Drop for Fixture {fn drop(&mut self){let _=fs::remove_dir_all(&self.0);}}
    #[test] fn copy_never_overwrites_or_recurses_into_itself() {
        let f=Fixture::new();let src=f.0.join("source");let dst=f.0.join("dest");fs::create_dir(&src).unwrap();fs::create_dir(&dst).unwrap();
        fs::write(src.join("a"),"original").unwrap();fs::write(dst.join("a"),"existing").unwrap();
        let cancel=AtomicBool::new(false);
        assert!(fileops::transfer(&src.join("a"),&dst,false,&cancel,&mut |_|{}).is_err());
        assert_eq!(fs::read_to_string(dst.join("a")).unwrap(),"existing");
        assert!(fileops::transfer(&src,&src,false,&cancel,&mut |_|{}).is_err());
        assert!(fileops::transfer(&src.join("a"),&src,false,&cancel,&mut |_|{}).is_err());
        fileops::transfer(&src,&dst,false,&cancel,&mut |_|{}).unwrap();
        assert_eq!(fs::read_to_string(dst.join("source/a")).unwrap(),"original");
    }
    #[test] fn cancellation_removes_partial_copy_and_preserves_source() {
        let f=Fixture::new();let dst=f.0.join("dest");fs::create_dir(&dst).unwrap();let src=f.0.join("large");fs::write(&src,vec![7;1024*1024]).unwrap();
        let cancel=AtomicBool::new(false);let mut bytes=0;
        assert!(fileops::transfer(&src,&dst,false,&cancel,&mut |n|{bytes+=n;cancel.store(true,Ordering::Release)}).is_err());
        assert!(bytes>0);assert_eq!(fs::metadata(&src).unwrap().len(),1024*1024);assert_eq!(fs::read_dir(dst).unwrap().count(),0);
    }
    #[test] fn symlinks_are_copied_not_followed_and_broken_links_move() {
        let f=Fixture::new();let src=f.0.join("source");let dst=f.0.join("dest");fs::create_dir(&src).unwrap();fs::create_dir(&dst).unwrap();
        std::os::unix::fs::symlink(".",src.join("loop")).unwrap();std::os::unix::fs::symlink("missing",src.join("broken")).unwrap();
        let cancel=AtomicBool::new(false);fileops::transfer(&src,&dst,false,&cancel,&mut |_|{}).unwrap();
        assert_eq!(fs::read_link(dst.join("source/loop")).unwrap(),PathBuf::from("."));
        fileops::transfer(&src.join("broken"),&dst,true,&cancel,&mut |_|{}).unwrap();assert!(fs::symlink_metadata(dst.join("broken")).unwrap().file_type().is_symlink());
    }
    #[test] fn trash_restores_original_path_without_overwriting_collisions() {
        let f=Fixture::new();let trash=f.0.join("trash");let src=f.0.join("résumé.txt");fs::write(&src,"keep me").unwrap();
        fileops::trash(&src,&trash).unwrap();assert!(!src.exists());let items=fileops::trash_items(&trash).unwrap();assert_eq!(items[0].name,"résumé.txt");
        fs::write(&src,"new content").unwrap();assert!(fileops::restore(&items[0],&trash).is_err());assert_eq!(fs::read_to_string(&src).unwrap(),"new content");
        fs::remove_file(&src).unwrap();fileops::restore(&items[0],&trash).unwrap();assert_eq!(fs::read_to_string(src).unwrap(),"keep me");assert!(fileops::trash_items(&trash).unwrap().is_empty());
    }
    #[test] fn invalid_names_and_existing_files_are_rejected() {
        let f=Fixture::new();for s in ["",".","..","../x","/tmp/x","x\0y"] {assert!(fileops::create_file(&f.0,s).is_err());}
        fileops::create_file(&f.0,"a").unwrap();fs::write(f.0.join("a"),"keep").unwrap();assert!(fileops::create_file(&f.0,"a").is_err());assert_eq!(fs::read_to_string(f.0.join("a")).unwrap(),"keep");
    }
    #[test] fn preview_is_bounded_and_unicode_safe_and_listing_errors_are_visible() {
        let f=Fixture::new();let p=f.0.join("large.txt");fs::write(&p,"界".repeat(100000)).unwrap();let preview=filebrowser::read_preview(&p,50);assert_eq!(preview.chars().count(),241);
        assert!(filebrowser::list_dir_checked("/not/a/real/directory",false,"","name",true).is_err());
        fs::write(f.0.join("small"),vec![0;101]).unwrap();fs::write(f.0.join("bigger"),vec![0;102]).unwrap();
        let list=filebrowser::list_dir_checked(f.0.to_str().unwrap(),false,"","size",true).unwrap();assert_eq!(list[0].name,"small");assert_eq!(list[1].name,"bigger");
    }
    #[test] fn preview_does_not_block_on_a_named_pipe() {
        use std::os::unix::ffi::OsStrExt;
        let f=Fixture::new();let p=f.0.join("pipe.txt");
        let name=std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe {libc::mkfifo(name.as_ptr(),0o600)},0);
        assert_eq!(filebrowser::read_preview(&p,50),"");
    }

}
