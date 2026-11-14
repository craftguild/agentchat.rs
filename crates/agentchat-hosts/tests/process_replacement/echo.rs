//! Native-free owned process; built normally, never patched or re-signed.
fn main() {
    std::io::copy(&mut std::io::stdin(), &mut std::io::stdout()).unwrap();
}
