fn main() {
    if cfg!(target_os = "windows") {
        let mut res = winres::WindowsResource::new();
        // Иконка будет добавлена как только будет ICO файл
        // res.set_icon("../../assets/icon.ico");
        res.set("FileDescription", "Cheburgram Voice Messenger");
        res.set("ProductName", "Cheburgram");
        res.set("LegalCopyright", "Cheburgram Team");
        res.set_version_info(winres::VersionInfo::PRODUCTVERSION, 0x0002000000000000);
        let _ = res.compile();
    }
}
