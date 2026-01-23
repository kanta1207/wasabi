#![no_std]
// Do not use the standard library. In OS/UEFI environments, the OS features required by `std` do not exist.
#![no_main] // Do not use Rust’s default `main()`. We define our own entry point that UEFI calls.

use core::arch::asm;
use core::cmp::min;
use core::mem::offset_of; // Used to verify struct field offsets (to ensure they match the C layout).
use core::mem::size_of; // Used for size calculations (e.g., converting framebuffer size to element count).
use core::panic::PanicInfo;
use core::ptr::null_mut; // Used to create null pointers for UEFI calls.

// Type aliases to minimally represent UEFI types on the Rust side.
// UEFI is a C-based world, so concepts like `void*` and `handle` appear.
type EfiVoid = u8; // Dummy representation of C's `void` (*mut EfiVoid == void*)
type EfiHandle = u64; // UEFI handle (treated as u64 in the book)
type Result<T> = core::result::Result<T, &'static str>; // Simple error handling for no_std environments.

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
#[must_use]
#[repr(u64)] // UEFI status codes are integers; we match the ABI by using u64.
enum EfiStatus {
    Success = 0, // Success code. Failure codes are omitted for now.
}

// --- Entry point called by UEFI ---
#[no_mangle] // Disable Rust name mangling so UEFI can find the symbol.
fn efi_main(_image_handle: EfiHandle, efi_system_table: &EfiSystemTable) {
    let mut vram = init_vram(efi_system_table).expect("init_vram failed");

    for y in 0..vram.height{
        for x in 0..vram.width{
            if let Some(pixel) = vram.pixel_at_mut(x, y){
                *pixel = 0x00ff00;
            }
        }
    }

    // Eventually we want to draw text here, but we do not have println yet.
    // println!("Hello, world!");
    loop {
        hlt();
    }
}
// --- UEFI table definitions (only the required parts) ---
// Purpose: Define the UEFI System Table and Boot Services Table in Rust
// using a C-compatible layout so we can call into UEFI.
// UEFI tables are huge, so we skip unused fields with reserved padding.
#[repr(C)] // Ensure the same memory layout as C (without this, field offsets may differ and crash).
struct EfiBootServicesTable {
    // Purpose: Skip all fields up to `locate_protocol`.
    // [u64; 40] = 40 * 8 = 320 bytes of padding.
    _reserved0: [u64; 40],

    // Purpose: Function pointer provided by UEFI Boot Services.
    // `locate_protocol` finds a protocol (e.g., GOP) by GUID and returns its interface.
    // `extern "win64"` matches the UEFI calling convention (MS ABI on x86_64).
    locate_protocol: extern "win64" fn(
        protocol: *const EfiGuid,     // GUID of the protocol to locate
        registration: *const EfiVoid, // Usually null (advanced usage only)
        interface: *mut *mut EfiVoid, // Out parameter: pointer to the protocol interface
    ) -> EfiStatus,
}

// Purpose: Verify at compile time that the field offset matches the expected UEFI layout.
// If this is wrong, we may end up calling a completely different function pointer.
const _: () = assert!(offset_of!(EfiBootServicesTable, locate_protocol) == 320);

#[repr(C)]
struct EfiSystemTable {
    // Purpose: Skip fields until `boot_services`.
    // 12 * 8 = 96 bytes.
    _reserved0: [u64; 12],

    // Purpose: Reference to the Boot Services Table.
    // From here, we can access UEFI service functions like `locate_protocol`.
    pub boot_services: &'static EfiBootServicesTable,
}

// Again, verify the field offset.
const _: () = assert!(offset_of!(EfiSystemTable, boot_services) == 96);

// --- GUID (protocol identifier) ---
// Purpose: Each UEFI protocol is identified by a GUID.
// To obtain GOP, we must pass the GOP GUID to `locate_protocol`.
// First, we define the GUID structure.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct EfiGuid {
    // A GUID consists of four parts:
    // data0: 32-bit integer
    // data1: 16-bit integer
    // data2: 16-bit integer
    // data3: 8-byte array
    pub data0: u32,
    pub data1: u16,
    pub data2: u16,
    pub data3: [u8; 8],
}

// The GOP GUID (fixed value defined by the UEFI specification).
const EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID: EfiGuid = EfiGuid {
    data0: 0x9042a9de,
    data1: 0x23dc,
    data2: 0x4a38,
    data3: [0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a],
};

// --- Minimal definition of GOP (Graphics Output Protocol) ---
// Purpose: Access `mode` to retrieve `frame_buffer_base` and `frame_buffer_size`.
// This is a minimal Rust reimplementation of the UEFI C structure.
#[repr(C)]
#[derive(Debug)]
struct EfiGraphicsOutputProtocol<'a> {
    // Purpose: Skip unused leading fields (e.g., function pointers).
    // At this stage, we only care about `mode`.
    reserved: [u64; 3],

    // Purpose: Pointer to the current graphics mode information.
    pub mode: &'a EfiGraphicsOutputProtocolMode<'a>,
}

#[repr(C)]
#[derive(Debug)]
struct EfiGraphicsOutputProtocolMode<'a> {
    // Purpose: Information about available and current modes.
    // `max_mode` is the number of supported modes.
    // `mode` is the current mode index.
    pub max_mode: u32,
    pub mode: u32,

    // Purpose: Pointer to resolution and pixel format information.
    pub info: &'a EfiGraphicsOutputProtocolPixelInfo,
    pub size_of_info: u64,

    // Purpose: Base address and size of the framebuffer (drawing memory).
    // Once we have these, we can draw by writing to memory.
    pub frame_buffer_base: usize,
    pub frame_buffer_size: usize,
}

#[repr(C)]
#[derive(Debug)]
struct EfiGraphicsOutputProtocolPixelInfo {
    // Purpose: Information needed for drawing (e.g., text or line rendering).
    version: u32,
    pub horizontal_resolution: u32,
    pub vertical_resolution: u32,

    // Purpose: Padding to match the UEFI structure layout.
    _padding0: [u32; 5],

    // Purpose: Number of pixels per scan line (stride).
    // This may differ from horizontal_resolution and is important for row calculations.
    pub pixels_per_scan_line: u32,
}

// Purpose: Verify that the structure size matches expectations.
// A size mismatch would shift subsequent fields and break everything.
const _: () = assert!(size_of::<EfiGraphicsOutputProtocolPixelInfo>() == 36);

// --- Locate GOP ---
// Purpose: Call System Table → Boot Services → locate_protocol to obtain GOP.
// This is the core interaction with UEFI to retrieve framebuffer information.
fn locate_graphic_protocol<'a>(
    efi_system_table: &EfiSystemTable,
) -> Result<&'a EfiGraphicsOutputProtocol<'a>> {
    // Out parameter to receive the GOP pointer from `locate_protocol`.
    let mut graphic_output_protocol = null_mut::<EfiGraphicsOutputProtocol>();

    // Purpose: Locate GOP by GUID.
    // `registration` is usually null.
    // `interface` is a void** in UEFI, so we cast accordingly.
    let status = (efi_system_table.boot_services.locate_protocol)(
        &EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID,
        null_mut::<EfiVoid>(),
        &mut graphic_output_protocol as *mut *mut EfiGraphicsOutputProtocol as *mut *mut EfiVoid,
    );

    // Purpose: Return an error if the protocol could not be found.
    if status != EfiStatus::Success {
        return Err("Failed to locate graphics output protocol");
    }

    // Purpose: Convert the raw pointer into a reference and return it.
    // Unsafe because Rust cannot guarantee that the pointer is valid or non-null.
    Ok(unsafe { &*graphic_output_protocol })
}

pub fn hlt() {
    unsafe { asm!("hlt") }
}

// --- Panic behavior ---
// In a no_std environment, there is no default panic output.
// We define a minimal panic handler that simply halts execution.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        hlt();
    }
}

trait Bitmap {
    fn bytes_per_pixel(&self) -> i64;
    fn pixels_per_line(&self) -> i64;
    fn width(&self) -> i64;
    fn height(&self) -> i64;
    fn buf_mut(&mut self) -> *mut u8;
    /// # Safety
    ///
    /// Returned pointer is valid as long as the given coordinates are valid
    /// which means that passing is_in_*_range tests.
    unsafe fn unchecked_pixel_at_mut(&mut self, x: i64, y: i64) -> *mut u32 {
        self.buf_mut().add(
            ((y * self.pixels_per_line() + x) * self.bytes_per_pixel())
                as usize,
        ) as *mut u32
    }
    fn pixel_at_mut(&mut self, x: i64, y: i64) -> Option<&mut u32> {
        if self.is_in_x_range(x) && self.is_in_y_range(y) {
            // SAFETY: (x, y) is always validated by the checks above.
            unsafe { Some(&mut *(self.unchecked_pixel_at_mut(x, y))) }
        } else {
            None
        }
    }
    fn is_in_x_range(&self, px: i64) -> bool {
        0 <= px && px < min(self.width(), self.pixels_per_line())
    }
    fn is_in_y_range(&self, py: i64) -> bool {
        0 <= py && py < self.height()
    }
}


#[derive(Clone, Copy)]
struct VramBufferInfo {
    buf: *mut u8,
    width: i64,
    height: i64,
    pixels_per_line: i64,
}

impl Bitmap for VramBufferInfo {
    fn bytes_per_pixel(&self) -> i64 {
        4
    }
    fn pixels_per_line(&self) -> i64 {
        self.pixels_per_line
    }
    fn width(&self) -> i64 {
        self.width
    }
    fn height(&self) -> i64 {
        self.height
    }
    fn buf_mut(&mut self) -> *mut u8 {
        self.buf
    }
}

fn init_vram(efi_system_table: &EfiSystemTable) -> Result<VramBufferInfo> {
    let gp = locate_graphic_protocol(efi_system_table)?;
    Ok(VramBufferInfo {
        buf: gp.mode.frame_buffer_base as *mut u8,
        width: gp.mode.info.horizontal_resolution as i64,
        height: gp.mode.info.vertical_resolution as i64,
        pixels_per_line: gp.mode.info.pixels_per_scan_line as i64,
    })
}
