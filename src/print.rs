use crate::BytesOrWideString;
use core::ffi::c_void;
use core::fmt;

const HEX_WIDTH: usize = 2 + 2 * core::mem::size_of::<usize>();

#[cfg(target_os = "fuchsia")]
mod fuchsia;

/// A formatter for backtraces.
///
/// This type can be used to print a backtrace regardless of where the backtrace
/// itself comes from. If you have a `Backtrace` type then its `Debug`
/// implementation already uses this printing format.
pub struct BacktraceFmt<'a, 'b> {
    fmt: &'a mut fmt::Formatter<'b>,
    frame_index: usize,
    format: PrintFmt,
    print_path: &'a mut (FnMut(&mut fmt::Formatter, BytesOrWideString) -> fmt::Result + 'b),
}

/// The styles of printing that we can print
pub enum PrintFmt {
    /// Prints a terser backtrace which ideally only contains relevant information
    Short,
    /// Prints a backtrace that contains all possible information
    Full,
    #[doc(hidden)]
    __Nonexhaustive,
}

impl<'a, 'b> BacktraceFmt<'a, 'b> {
    /// Create a new `BacktraceFmt` which will write output to the provided
    /// `fmt`.
    ///
    /// The `format` argument will control the style in which the backtrace is
    /// printed, and the `print_path` argument will be used to print the
    /// `BytesOrWideString` instances of filenames. This type itself doesn't do
    /// any printing of filenames, but this callback is required to do so.
    pub fn new(
        fmt: &'a mut fmt::Formatter<'b>,
        format: PrintFmt,
        print_path: &'a mut (FnMut(&mut fmt::Formatter, BytesOrWideString) -> fmt::Result + 'b),
    ) -> Self {
        BacktraceFmt {
            fmt,
            frame_index: 0,
            format,
            print_path,
        }
    }

    /// Prints a preamble for the backtrace about to be printed.
    ///
    /// This is required on some platforms for backtraces to be fully
    /// sumbolicated later, and otherwise this should just be the first method
    /// you call after creating a `BacktraceFmt`.
    pub fn add_context(&mut self) -> fmt::Result {
        self.fmt.write_str("stack backtrace:\n")?;
        #[cfg(target_os = "fuchsia")]
        fuchsia::print_dso_context(self.fmt)?;
        Ok(())
    }

    /// Adds a frame to the backtrace output.
    ///
    /// This commit returns an RAII instance of a `BacktraceFrameFmt` which can be used
    /// to actually print a frame, and on destruction it will increment the
    /// frame counter.
    pub fn frame(&mut self) -> BacktraceFrameFmt<'_, 'a, 'b> {
        BacktraceFrameFmt {
            fmt: self,
            symbol_index: 0,
        }
    }

    /// Completes the backtrace output.
    ///
    /// This is currently a no-op but is added for future compatibility with
    /// backtrace formats.
    pub fn finish(&mut self) -> fmt::Result {
        // Currently a no-op-- including this hook to allow for future additions.
        Ok(())
    }
}

/// A formatter for just one frame of a backtrace.
///
/// This type is created by the `BacktraceFmt::frame` function.
pub struct BacktraceFrameFmt<'fmt, 'a, 'b> {
    fmt: &'fmt mut BacktraceFmt<'a, 'b>,
    symbol_index: usize,
}

impl BacktraceFrameFmt<'_, '_, '_> {
    /// Prints a `BacktraceFrame` with this frame formatter.
    ///
    /// This will recusrively print all `BacktraceSymbol` instances within the
    /// `BacktraceFrame`.
    #[cfg(feature = "std")]
    pub fn backtrace_frame(&mut self, frame: &crate::BacktraceFrame) -> fmt::Result {
        let symbols = frame.symbols();
        for symbol in symbols {
            self.backtrace_symbol(frame, symbol)?;
        }
        if symbols.is_empty() {
            self.print_raw(frame.ip(), None, None, None)?;
        }
        Ok(())
    }

    /// Prints a `BacktraceSymbol` within a `BacktraceFrame`.
    #[cfg(feature = "std")]
    pub fn backtrace_symbol(
        &mut self,
        frame: &crate::BacktraceFrame,
        symbol: &crate::BacktraceSymbol,
    ) -> fmt::Result {
        self.print_raw(
            frame.ip(),
            symbol.name(),
            // TODO: this isn't great that we don't end up printing anything
            // with non-utf8 filenames. Thankfully almost everything is utf8 so
            // this shouldn't be too too bad.
            symbol
                .filename()
                .and_then(|p| Some(BytesOrWideString::Bytes(p.to_str()?.as_bytes()))),
            symbol.lineno(),
        )?;
        Ok(())
    }

    /// Prints a raw traced `Frame` and `Symbol`, typically from within the raw
    /// callbacks of this crate.
    pub fn symbol(&mut self, frame: &crate::Frame, symbol: &crate::Symbol) -> fmt::Result {
        self.print_raw(
            frame.ip(),
            symbol.name(),
            symbol.filename_raw(),
            symbol.lineno(),
        )?;
        Ok(())
    }

    /// Adds a raw frame to the backtrace output.
    ///
    /// This method, unlike the previous, takes the raw arguments in case
    /// they're being source from different locations. Note that this may be
    /// called multiple times for one frame.
    pub fn print_raw(
        &mut self,
        frame_ip: *mut c_void,
        symbol_name: Option<crate::SymbolName>,
        filename: Option<BytesOrWideString>,
        lineno: Option<u32>,
    ) -> fmt::Result {
        // Fuchsia is unable to symbolize within a process so it has a special
        // format which can be used to symbolize later. Print that instead of
        // printing addresses in our own format here.
        if cfg!(target_os = "fuchsia") {
            self.print_raw_fuchsia(frame_ip)?;
        } else {
            self.print_raw_generic(frame_ip, symbol_name, filename, lineno)?;
        }
        self.symbol_index += 1;
        Ok(())
    }

    #[allow(unused_mut)]
    fn print_raw_generic(
        &mut self,
        mut frame_ip: *mut c_void,
        symbol_name: Option<crate::SymbolName>,
        filename: Option<BytesOrWideString>,
        lineno: Option<u32>,
    ) -> fmt::Result {
        // No need to print "null" frames, it basically just means that the
        // system backtrace was a bit eager to trace back super far.
        if let PrintFmt::Short = self.fmt.format {
            if frame_ip.is_null() {
                return Ok(());
            }
        }

        // To reduce TCB size in Sgx enclave, we do not want to implement symbol
        // resolution functionality.  Rather, we can print the offset of the
        // address here, which could be later mapped to correct function.
        #[cfg(all(feature = "std", target_env = "sgx"))]
        {
            frame_ip =
                usize::wrapping_sub(frame_ip, std::os::fortanix_sgx::mem::image_base() as _) as _;
        }

        // Print the index of the frame as well as the optional instruction
        // pointer of the frame. If we're beyond the first symbol of this frame
        // though we just print appropriate whitespace.
        if self.symbol_index == 0 {
            write!(self.fmt.fmt, "{:4}: ", self.fmt.frame_index)?;
            if let PrintFmt::Full = self.fmt.format {
                write!(self.fmt.fmt, "{:1$?} - ", frame_ip, HEX_WIDTH)?;
            }
        } else {
            write!(self.fmt.fmt, "      ")?;
            if let PrintFmt::Full = self.fmt.format {
                write!(self.fmt.fmt, "{:1$}", "", HEX_WIDTH + 3)?;
            }
        }

        // Next up write out the symbol name, using the alternate formatting for
        // more information if we're a full backtrace. Here we also handle
        // symbols which don't have a name,
        match (symbol_name, &self.fmt.format) {
            (Some(name), PrintFmt::Short) => write!(self.fmt.fmt, "{}", name)?,
            (Some(name), PrintFmt::Full) => write!(self.fmt.fmt, "{:#}", name)?,
            (None, _) | (_, PrintFmt::__Nonexhaustive) => write!(self.fmt.fmt, "<unknown>")?,
        }
        self.fmt.fmt.write_str("\n")?;

        // And last up, print out the filename/line number if they're available.
        if let (Some(file), Some(line)) = (filename, lineno) {
            self.print_fileline(file, line)?;
        }

        Ok(())
    }

    fn print_fileline(&mut self, file: BytesOrWideString, line: u32) -> fmt::Result {
        // Filename/line are printed on lines under the symbol name, so print
        // some appropriate whitespace to sort of right-align ourselves.
        if let PrintFmt::Full = self.fmt.format {
            write!(self.fmt.fmt, "{:1$}", "", HEX_WIDTH)?;
        }
        write!(self.fmt.fmt, "             at ")?;

        // Delegate to our internal callback to print the filename and then
        // print out the line number.
        (self.fmt.print_path)(self.fmt.fmt, file)?;
        write!(self.fmt.fmt, ":{}\n", line)?;
        Ok(())
    }

    fn print_raw_fuchsia(&mut self, frame_ip: *mut c_void) -> fmt::Result {
        // We only care about the first symbol of a frame
        if self.symbol_index == 0 {
            self.fmt.fmt.write_str("{{{bt:")?;
            write!(self.fmt.fmt, "{}:{:?}", self.fmt.frame_index, frame_ip)?;
            self.fmt.fmt.write_str("}}}\n")?;
        }
        Ok(())
    }
}

impl Drop for BacktraceFrameFmt<'_, '_, '_> {
    fn drop(&mut self) {
        self.fmt.frame_index += 1;
    }
}
