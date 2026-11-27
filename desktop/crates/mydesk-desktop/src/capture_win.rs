//! Windows 上的抓屏热路径：直接用 GDI `BitBlt`。
//!
//! 为什么不用 xcap 抓帧：xcap 的 `Monitor::all()` 每次都要重新枚举显示器，
//! 抓一帧实测 1.9 s（还把缩放和 JPEG 串在一起），而且放到工作线程里会抓不到
//! （Windows.Graphics.Capture 需要 COM 初始化 + 消息泵）。GDI 的 BitBlt
//! 在 2560×1440 上只要十几毫秒，任何线程都能用，也不依赖 COM。
//!
//! xcap 只保留给「枚举显示器名字/尺寸」这种一次性调用。

#![cfg(windows)]

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, SetBrushOrgEx, SetStretchBltMode, StretchBlt, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HALFTONE, SRCCOPY,
};

/// 抓一块屏幕区域，返回 BGRA（自上而下）像素。
pub fn capture_bgra(x: i32, y: i32, width: i32, height: i32) -> Option<Vec<u8>> {
    if width <= 0 || height <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(std::ptr::null_mut() as HWND);
        if screen.is_null() {
            return None;
        }
        let mem = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        if mem.is_null() || bitmap.is_null() {
            let _ = ReleaseDC(std::ptr::null_mut() as HWND, screen);
            return None;
        }
        let old = SelectObject(mem, bitmap);
        // CAPTUREBLT 让分层窗口也进画面
        let blitted = BitBlt(mem, 0, 0, width, height, screen, x, y, SRCCOPY | CAPTUREBLT);

        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // 负值 = 自上而下，省一次翻转
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut buf = vec![0u8; (width as usize) * (height as usize) * 4];
        let lines = GetDIBits(
            mem,
            bitmap,
            0,
            height as u32,
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            &mut info,
            DIB_RGB_COLORS,
        );

        SelectObject(mem, old);
        DeleteObject(bitmap);
        DeleteDC(mem);
        ReleaseDC(std::ptr::null_mut() as HWND, screen);

        if blitted == 0 || lines == 0 {
            return None;
        }
        Some(buf)
    }
}

/// 抓屏 + 缩放一次完成：缩放交给显卡/驱动（StretchBlt），比在 CPU 上做
/// Triangle 重采样快一个数量级，也省掉一次全尺寸内存拷贝。
pub fn capture_scaled_bgra(
    src_x: i32,
    src_y: i32,
    src_w: i32,
    src_h: i32,
    dst_w: i32,
    dst_h: i32,
) -> Option<Vec<u8>> {
    if dst_w <= 0 || dst_h <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(std::ptr::null_mut() as HWND);
        if screen.is_null() {
            return None;
        }
        let mem = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, dst_w, dst_h);
        if mem.is_null() || bitmap.is_null() {
            let _ = ReleaseDC(std::ptr::null_mut() as HWND, screen);
            return None;
        }
        let old = SelectObject(mem, bitmap);
        SetStretchBltMode(mem, HALFTONE);
        let _ = SetBrushOrgEx(mem, 0, 0, std::ptr::null_mut());
        let scaled = StretchBlt(
            mem, 0, 0, dst_w, dst_h, screen, src_x, src_y, src_w, src_h,
            SRCCOPY | CAPTUREBLT,
        );

        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: dst_w,
            biHeight: -dst_h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut buf = vec![0u8; (dst_w as usize) * (dst_h as usize) * 4];
        let lines = GetDIBits(
            mem,
            bitmap,
            0,
            dst_h as u32,
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            &mut info,
            DIB_RGB_COLORS,
        );

        SelectObject(mem, old);
        DeleteObject(bitmap);
        DeleteDC(mem);
        ReleaseDC(std::ptr::null_mut() as HWND, screen);

        if scaled == 0 || lines == 0 {
            return None;
        }
        Some(buf)
    }
}

/// BGRA → RGBA（原地交换 R/B）
pub fn bgra_to_rgba(mut buf: Vec<u8>) -> Vec<u8> {
    for px in buf.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    buf
}
