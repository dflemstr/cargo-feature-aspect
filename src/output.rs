/// Whether to color logged output
pub fn colorize_stderr() -> termcolor::ColorChoice {
    if concolor_control::get(concolor_control::Stream::Stderr).color() {
        termcolor::ColorChoice::Always
    } else {
        termcolor::ColorChoice::Never
    }
}

/// Print a message with a colored title in the style of Cargo shell messages.
pub fn shell_print(
    status: &str,
    message: &str,
    color: termcolor::Color,
    justified: bool,
) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use std::io::Write as _;
    use termcolor::WriteColor as _;

    let color_choice = colorize_stderr();
    let mut output = termcolor::StandardStream::stderr(color_choice);

    output.set_color(
        termcolor::ColorSpec::new()
            .set_fg(Some(color))
            .set_bold(true),
    )?;
    if justified {
        write!(output, "{status:>12}")?;
    } else {
        write!(output, "{status}")?;
        output.set_color(termcolor::ColorSpec::new().set_bold(true))?;
        write!(output, ":")?;
    }
    output.reset()?;

    writeln!(output, " {message}").with_context(|| "Failed to write message")?;

    Ok(())
}

/// Print a styled action message.
pub fn shell_status(action: &str, message: &str) -> anyhow::Result<()> {
    shell_print(action, message, termcolor::Color::Green, true)
}
