/*!
This module is for platform-specific stuff.
*/

pub use self::inner::{
    current_time, file_last_modified, get_cache_dir, get_config_dir,
    write_path, read_path,
    force_cargo_color,
};

#[cfg(any(unix, windows))]
mod inner_unix_or_windows {
    extern crate time;

    /**
    Gets the current system time, in milliseconds since the UNIX epoch.
    */
    pub fn current_time() -> u64 {
        /*
        This is kinda dicey, since *ideally* both this function and `file_last_modified` would be using the same underlying APIs.  They are not, insofar as I know.

        At least, not when targetting Windows.

        That said, so long as everything is in the same units and uses the same epoch, it should be fine.
        */
        let now_1970_utc = time::now_utc().to_timespec();
        if now_1970_utc.sec < 0 || now_1970_utc.nsec < 0 {
            // Fuck it.
            return 0
        }
        (now_1970_utc.sec as u64 * 1000)
            + (now_1970_utc.nsec as u64 / 1_000_000)
    }
}

#[cfg(unix)]
mod inner {
    extern crate atty;

    pub use super::inner_unix_or_windows::current_time;

    use std::path::{Path, PathBuf};
    use std::{cmp, env, fs, io};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use crate::error::{MainError, Blame};

    /**
    Gets the last-modified time of a file, in milliseconds since the UNIX epoch.
    */
    pub fn file_last_modified(file: &fs::File) -> u64 {
        let mtime_s_1970_utc = file.metadata()
            .map(|md| md.mtime())
            .unwrap_or(0);

        let mtime_s_1970_utc = cmp::max(0, mtime_s_1970_utc);
        mtime_s_1970_utc as u64 * 1000
    }

    /**
    Get a directory suitable for storing user- and machine-specific data which may or may not be persisted across sessions.

    This is chosen to match the location where Cargo places its cache data.
    */
    pub fn get_cache_dir() -> Result<PathBuf, MainError> {
        // try $CARGO_HOME then fall back to $HOME
        if let Some(home) = env::var_os("CARGO_HOME") {
            let home = Path::new(&home);
            let old_home = home.join(".cargo");
            if old_home.exists() {
                // Keep using the old directory in preference to the new one, but only if it still contains `script-cache` and/or `binary-cache`.
                if old_home.join("script-cache").exists() || old_home.join("binary-cache").exists() {
                    // Yup; use this one.
                    return Ok(old_home);
                }
            }

            // Just use `$CARGO_HOME` directly.
            return Ok(home.into());
        }

        if let Some(home) = env::var_os("HOME") {
            return Ok(Path::new(&home).join(".cargo"));
        }

        Err((Blame::Human, "neither $CARGO_HOME nor $HOME is defined").into())
    }

    /**
    Get a directory suitable for storing user-specific configuration data.

    This is chosen to match the location where Cargo places its configuration data.
    */
    pub fn get_config_dir() -> Result<PathBuf, MainError> {
        // Currently, this appears to be the same as the cache directory.
        get_cache_dir()
    }

    pub fn write_path<W>(w: &mut W, path: &Path) -> io::Result<()>
    where W: io::Write {
        w.write_all(path.as_os_str().as_bytes())
    }

    pub fn read_path<R>(r: &mut R) -> io::Result<PathBuf>
    where R: io::Read {
        use std::ffi::OsStr;
        let mut buf = vec![];
        r.read_to_end(&mut buf)?;
        Ok(OsStr::from_bytes(&buf).into())
    }

    /**
    Returns `true` if `cargo-eval` should force Cargo to use coloured output.

    This depends on whether `cargo-eval`'s STDERR is connected to a TTY or not.
    */
    pub fn force_cargo_color() -> bool {
        atty::is(atty::Stream::Stderr)
    }
}

#[cfg(windows)]
pub mod inner {
    #![allow(non_snake_case)]

    pub use super::inner_unix_or_windows::current_time;

    use std::ffi::OsString;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::mem;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use crate::error::MainError;

    use winapi::{
      shared::{
        minwindef::DWORD,
        ntdef::{HANDLE, PWSTR},
        winerror::{HRESULT, S_OK},
      },
      um::{
        combaseapi::CoTaskMemFree,
        shlobj::SHGetKnownFolderPath,
        shtypes::KNOWNFOLDERID,
        knownfolders::{FOLDERID_LocalAppData, FOLDERID_RoamingAppData},
      },
    };

    /**
    Gets the last-modified time of a file, in milliseconds since the UNIX epoch.
    */
    pub fn file_last_modified(file: &fs::File) -> u64 {
        use ::std::os::windows::fs::MetadataExt;

        const MS_BETWEEN_1601_1970: u64 = 11_644_473_600_000;

        let mtime_100ns_1601_utc = file.metadata()
            .map(|md| md.last_write_time())
            .unwrap_or(0);
        let mtime_ms_1601_utc = mtime_100ns_1601_utc / (1000*10);

        // This can obviously underflow... but since files created prior to 1970 are going to be *somewhat rare*, I'm just going to saturate to zero.
        let mtime_ms_1970_utc = mtime_ms_1601_utc.saturating_sub(MS_BETWEEN_1601_1970);
        mtime_ms_1970_utc
    }

    /**
    Get a directory suitable for storing user- and machine-specific data which may or may not be persisted across sessions.

    This is *not* chosen to match the location where Cargo places its cache data, because Cargo is *wrong*.  This is at least *less wrong*.

    On Windows, LocalAppData is where user- and machine- specific data should go, but it *might* be more appropriate to use whatever the official name for "Program Data" is, though.
    */
    pub fn get_cache_dir() -> Result<PathBuf, MainError> {
        let rfid = &FOLDERID_LocalAppData;
        let dir = sh_get_known_folder_path(rfid, 0, ::std::ptr::null_mut())
            .map_err(|e| e.to_string())?;
        Ok(Path::new(&dir).to_path_buf().join("Cargo"))
    }

    /**
    Get a directory suitable for storing user-specific configuration data.

    This is *not* chosen to match the location where Cargo places its cache data, because Cargo is *wrong*.  This is at least *less wrong*.
    */
    pub fn get_config_dir() -> Result<PathBuf, MainError> {
        let rfid = &FOLDERID_RoamingAppData;
        let dir = sh_get_known_folder_path(rfid, 0, ::std::ptr::null_mut())
            .map_err(|e| e.to_string())?;
        Ok(Path::new(&dir).to_path_buf().join("Cargo"))
    }

    fn sh_get_known_folder_path(rfid: &KNOWNFOLDERID, dwFlags: DWORD, hToken: HANDLE) -> Result<OsString, HRESULT> {
        let mut psz_path: PWSTR = unsafe { mem::uninitialized() };
        let hresult = unsafe {
            SHGetKnownFolderPath(
                rfid,
                dwFlags,
                hToken,
                mem::transmute(&mut psz_path as &mut PWSTR as *mut PWSTR)
            )
        };

        if hresult == S_OK {
            let r = unsafe { pwstr_to_os_string(psz_path) };
            unsafe { CoTaskMemFree(psz_path as *mut _) };
            Ok(r)
        } else {
            Err(hresult)
        }
    }

    unsafe fn pwstr_to_os_string(ptr: PWSTR) -> OsString {
        OsStringExt::from_wide(::std::slice::from_raw_parts(ptr, pwstr_len(ptr)))
    }

    unsafe fn pwstr_len(mut ptr: PWSTR) -> usize {
        let mut len = 0;
        while *ptr != 0 {
            len += 1;
            ptr = ptr.offset(1);
        }
        len
    }

    pub fn write_path<W>(w: &mut W, path: &Path) -> io::Result<()>
    where W: io::Write {
        for word in path.as_os_str().encode_wide() {
            let lo = (word & 0xff) as u8;
            let hi = (word >> 8) as u8;
            w.write_all(&[lo, hi])?;
        }
        Ok(())
    }

    pub fn read_path<R>(r: &mut R) -> io::Result<PathBuf>
    where R: io::Read {
        let mut buf = vec![];
        r.read_to_end(&mut buf)?;

        let mut words = Vec::with_capacity(buf.len() / 2);
        let mut it = buf.iter().cloned();
        while let Some(lo) = it.next() {
            let hi = it.next().unwrap();
            words.push(lo as u16 | ((hi as u16) << 8));
        }

        return Ok(OsString::from_wide(&words).into())
    }

    /**
    Returns `true` if `cargo-eval` should force Cargo to use coloured output.

    Always returns `false` on Windows because colour is communicated over a side-channel.
    */
    pub fn force_cargo_color() -> bool {
        false
    }
}
