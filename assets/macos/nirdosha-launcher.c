/* Nirdosha.app's "executable" -- a real Mach-O binary is a hard
 * requirement for a valid macOS app bundle (LSUIElement/LSBackgroundOnly
 * alone don't waive it), but this bundle only exists to register the
 * ".nir" file association + icon with Launch Services (see ../Info.plist's
 * comment). If Finder ever actually launches it -- double-clicking a
 * .nir file with no other handler set -- it should do something honest,
 * not silently vanish: say what it is and exit clean.
 *
 * Compiled by .github/workflows/release.yml's macOS jobs via the system
 * `cc` (Xcode command-line tools, present on every GitHub macOS runner)
 * into Contents/MacOS/nirdosha-launcher.
 */
#include <stdio.h>

int main(void) {
    fprintf(stderr,
        "Nirdosha.app is not a real application -- it only registers the "
        ".nir file icon with macOS. Use the `nirdosha` CLI instead:\n"
        "  nirdosha serve <file.nir>\n"
        "  nirdosha emit-ui <file.nir>\n"
        "See https://github.com/kannamma-labs/nirdosha\n");
    return 0;
}
