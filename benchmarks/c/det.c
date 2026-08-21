#include <math.h>
#include <stdio.h>

// Gaussian elimination with partial pivoting -- the exact algorithm
// compiler/src/interpreter.rs's matrix_det() uses, translated directly,
// so results are bit-for-bit comparable with Nirdosha (not just
// "close," the way Julia's LAPACK-based det() is).
double det4(double a[16]) {
    int n = 4;
    double d = 1.0;
    for (int col = 0; col < n; col++) {
        int pivot_row = col;
        double max_val = fabs(a[col * n + col]);
        for (int row = col + 1; row < n; row++) {
            double v = fabs(a[row * n + col]);
            if (v > max_val) {
                max_val = v;
                pivot_row = row;
            }
        }
        if (max_val == 0.0) {
            return 0.0;
        }
        if (pivot_row != col) {
            for (int k = 0; k < n; k++) {
                double tmp = a[col * n + k];
                a[col * n + k] = a[pivot_row * n + k];
                a[pivot_row * n + k] = tmp;
            }
            d = -d;
        }
        d *= a[col * n + col];
        for (int row = col + 1; row < n; row++) {
            double factor = a[row * n + col] / a[col * n + col];
            for (int k = col; k < n; k++) {
                a[row * n + k] -= factor * a[col * n + k];
            }
        }
    }
    return d;
}

int main(void) {
    long n_max = 200000;
    double t = 0.0;
    double checksum = 0.0;
    for (long n = 0; n < n_max; n++) {
        double m[16] = {
            t + 4.0, t + 1.0, t + 2.0, t + 0.5,
            t + 1.0, t + 5.0, t + 0.3, t + 1.2,
            t + 2.0, t + 0.3, t + 6.0, t + 0.7,
            t + 0.5, t + 1.2, t + 0.7, t + 7.0,
        };
        checksum += det4(m);
        t += 0.0001;
    }
    printf("%f\n", checksum);
    return 0;
}
