//! charts_save plugin: the charts renderer with write access to the bound
//! folder, so a chart can be saved as chart.svg and chart.png. The behaviour
//! is the charts library's; this crate only declares the stronger grant.
fn main() {
    charts::cli::main()
}
