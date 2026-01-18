#![no_std]  // 標準ライブラリ(std)を使わない。OS/UEFI環境ではstdが前提とするOS機能が無いから。
#![no_main] // Rust標準のmain()を使わない。UEFIが呼ぶエントリポイントを自分で定義するから。

use core::mem::offset_of; // 構造体フィールドのオフセット検証に使う（Cレイアウトと一致してるか確認）。
use core::mem::size_of;   // サイズ計算（framebufferサイズ→要素数に変換）に使う。
use core::panic::PanicInfo;
use core::ptr::null_mut;  // UEFI呼び出し時のnullポインタ生成に使う。
use core::slice;          // 生ポインタからスライスを作る（framebufferを配列として扱う）。

// UEFIの型をRust側で最小限に表現するための型エイリアス。
// UEFIはC系の世界なので「void*」「handle」みたいな概念が出る。
type EfiVoid = u8;     // Cで言う void のダミー表現（*mut EfiVoid == void* 相当）
type EfiHandle = u64;  // UEFIのハンドル（本ではu64で扱ってる）
type Result<T> = core::result::Result<T, &'static str>; // 例外がないno_std環境で簡易エラー返しをするため。

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
#[must_use]
#[repr(u64)] // UEFIのStatusは整数値（ここではu64扱い）としてABIに合わせる。
enum EfiStatus {
    Success = 0, // 成功コード。失敗コードは今は省略してる（必要になったら追加）。
}

// --- ここが「UEFIが呼び出す入口」 ---
#[no_mangle] // Rustの名前マングリングを無効化して、UEFI側がシンボル名を見つけられるようにする。
fn efi_main(_image_handle: EfiHandle, efi_system_table: &EfiSystemTable) {
    // 目的：UEFIが提供する「Graphics Output Protocol(GOP)」を探して取得する。
    // GOPが取れると、フレームバッファ（画面ピクセルの生メモリ）のアドレス等がわかる。
    let efi_graphics_output_protocol = locate_graphic_protocol(efi_system_table).unwrap();

    // 目的：フレームバッファ（VRAM相当）の開始アドレスとサイズを得る。
    let vram_addr = efi_graphics_output_protocol.mode.frame_buffer_base;
    let vram_byte_size = efi_graphics_output_protocol.mode.frame_buffer_size;

    // 目的：生アドレス（usize）を「u32配列」として扱えるようにする。
    //
    // - GOPのフレームバッファは多くの環境で 32bpp（1ピクセル=4バイト）なので u32 として塗れる。
    // - unsafeなのは「このアドレスが本当に有効で、かつこの長さ分書き込んで良い」保証がRustには無いから。
    let vram = unsafe {
        slice::from_raw_parts_mut(
            vram_addr as *mut u32,                  // 先頭アドレス
            vram_byte_size / size_of::<u32>(),      // 要素数（バイト→u32個数）
        )
    };

    // 目的：全ピクセルを白(0xFFFFFFFF)で塗りつぶして「画面に何か出た」を確認する。
    // これが「printlnの代替を作る前に、描画できることをまず確認」のステップ。
    for e in vram {
        *e = 0xFFFFFFFF;
    }

    // ここで「本当は文字描画」をやりたいが、まだprintln相当は無いのでコメントアウト。
    // println!("Hello, world!");
    loop {}
}

// --- UEFIのテーブル定義（必要な部分だけ） ---
// 目的：UEFIの「System Table」「Boot Services Table」をRustから叩くために
// C互換レイアウトで構造体を定義する。
// UEFIは巨大な構造体を持つので、必要なフィールドだけを「予約領域スキップ」で表現している。
#[repr(C)] // Cと同じメモリレイアウトにする（これが無いとフィールド配置がズレて即クラッシュし得る）。
struct EfiBootServicesTable {
    // 目的：locate_protocolが来るまでのフィールドを丸ごとスキップする。
    // [u64; 40] = 40*8 = 320バイト分の「穴」。
    _reserved0: [u64; 40],

    // 目的：UEFIのBoot Servicesが提供する関数ポインタ。
    // locate_protocolは「指定GUIDのプロトコル（GOPなど）を見つけて、そのインタフェースを返す」ために使う。
    // extern "win64" はUEFIの呼び出し規約に合わせるため（x86_64 UEFIではMS ABI寄りになる）。
    locate_protocol: extern "win64" fn(
        protocol: *const EfiGuid,     // 取得したいプロトコルのGUID
        registration: *const EfiVoid, // 通常はnull（高度な使い方をしないならnull）
        interface: *mut *mut EfiVoid, // 見つかったプロトコルのポインタがここに返る（out param）
    ) -> EfiStatus,
}

// 目的：フィールドオフセットが本の想定（=UEFI仕様の配置）と一致しているかをコンパイル時に検証。
// ずれてたら「UEFIの別の関数ポインタを呼ぶ」事故になるので、ビルド時に落として防ぐ。
const _: () = assert!(offset_of!(EfiBootServicesTable, locate_protocol) == 320);

#[repr(C)]
struct EfiSystemTable {
    // 目的：boot_servicesフィールドが来るまでの領域をスキップする。
    // 12*8 = 96バイト分。
    _reserved0: [u64; 12],

    // 目的：Boot Services Tableへの参照を持つ。
    // ここから locate_protocol などのUEFIサービス関数にアクセスできる。
    pub boot_services: &'static EfiBootServicesTable,
}

// 同じく、boot_servicesの位置が正しいか検証。
const _: () = assert!(offset_of!(EfiSystemTable, boot_services) == 96);

// --- GUID（プロトコル識別子） ---
// 目的：UEFIの各プロトコルはGUIDで識別される。
// GOPを取るには「GOPのGUID」を指定して locate_protocol を呼ぶ必要がある。
// そのためまず、GUID構造体を定義するところから。
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct EfiGuid {
    // GUIDは4つの16ビット整数（data0, data1, data2, data3）で構成される。
    // data0: 32ビット整数
    // data1: 16ビット整数
    // data2: 16ビット整数
    // data3: 8バイトの配列
    pub data0: u32,
    pub data1: u16,
    pub data2: u16,
    pub data3: [u8; 8],
}

// GOPのGUID（UEFI仕様で決まっている値）。
const EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID: EfiGuid = EfiGuid {
    data0: 0x9042a9de,
    data1: 0x23dc,
    data2: 0x4a38,
    data3: [0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a],
};

// --- GOP（Graphics Output Protocol）の最小定義 ---
// 目的：GOP構造体の中から「mode」を辿って framebuffer_base / framebuffer_size を得る。
// ここもUEFIのC構造体をRustで最小限再現している。
#[repr(C)]
#[derive(Debug)]
struct EfiGraphicsOutputProtocol<'a> {
    // 目的：先頭にある不要フィールド（関数ポインタ等）をスキップする。
    // 本の段階では「modeだけ欲しい」のでreservedにしている。
    reserved: [u64; 3],

    // 目的：現在のグラフィックモード情報へのポインタ。
    pub mode: &'a EfiGraphicsOutputProtocolMode<'a>,
}

#[repr(C)]
#[derive(Debug)]
struct EfiGraphicsOutputProtocolMode<'a> {
    // 目的：現在のモードに関する情報。
    // max_mode / mode は「モード数」「現在モード番号」等。
    pub max_mode: u32,
    pub mode: u32,

    // 目的：解像度やピクセル形式情報へのポインタ。
    pub info: &'a EfiGraphicsOutputProtocolPixelInfo,
    pub size_of_info: u64,

    // 目的：フレームバッファ（描画メモリ）の先頭アドレスとサイズ。
    // ここを取れれば「メモリ書き換えで画面を塗れる」。
    pub frame_buffer_base: usize,
    pub frame_buffer_size: usize,
}

#[repr(C)]
#[derive(Debug)]
struct EfiGraphicsOutputProtocolPixelInfo {
    // 目的：解像度など、描画時に必要になる情報（文字描画やライン描画で使う）。
    version: u32,
    pub horizontal_resolution: u32,
    pub vertical_resolution: u32,

    // 目的：構造体サイズ/配置をUEFI仕様に合わせるためのパディング。
    // 本では必要なフィールド以外をまとめて捨てている。
    _padding0: [u32; 5],

    // 目的：1行あたりのピクセル数（解像度の横幅と同じとは限らない）。
    // 次の行に移るときの計算で重要になる（stride相当）。
    pub pixels_per_scan_line: u32,
}

// 目的：構造体サイズが想定どおりかを検証。
// サイズが違うと後続フィールドの位置がズレるので危険。
const _: () = assert!(size_of::<EfiGraphicsOutputProtocolPixelInfo>() == 36);

// --- GOPを探す処理 ---

// 目的：System Table → Boot Services → locate_protocol を呼び、GOPのインターフェースポインタを得る。
// ここが「UEFIとやり取りしてフレームバッファ情報を取る」中心。
fn locate_graphic_protocol<'a>(
    efi_system_table: &EfiSystemTable,
) -> Result<&'a EfiGraphicsOutputProtocol<'a>> {
    // locate_protocol が返す「GOPのポインタ」を受け取るための変数（out param）。
    let mut graphic_output_protocol = null_mut::<EfiGraphicsOutputProtocol>();

    // 目的：GUID指定でGOPを検索し、見つかったら interface にポインタが入る。
    // registrationは通常null。
    // interfaceは void** なので型を合わせるためにキャストしている。
    let status = (efi_system_table.boot_services.locate_protocol)(
        &EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID,
        null_mut::<EfiVoid>(),
        &mut graphic_output_protocol as *mut *mut EfiGraphicsOutputProtocol
            as *mut *mut EfiVoid,
    );

    // 目的：失敗したらエラー。
    if status != EfiStatus::Success {
        return Err("Failed to locate graphics output protocol");
    }

    // 目的：取得した生ポインタを参照に変換して返す。
    // unsafe理由：nullじゃないこと・正しい型であることをRustが保証できないため。
    Ok(unsafe { &*graphic_output_protocol })
}

// --- panic時の挙動 ---
// no_std環境ではpanicの出力先が無いので、最低限「止まる」だけのpanic handlerを用意する。
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
