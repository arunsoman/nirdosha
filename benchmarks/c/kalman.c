#include <math.h>
#include <stdio.h>

// Direct translation of compiler/src/interpreter.rs's kf_predict/kf_update
// and matrix_inv (Gauss-Jordan with partial pivoting) -- same algorithm,
// same operation order, as the Nirdosha interpreter uses, so results are
// bit-for-bit comparable (not just "close," the way Julia's LAPACK-backed
// `inv`/`*` are).

// C = A(ar x ac) * B(ac x bc), row-major.
void mat_mul(const double *a, int ar, int ac, const double *b, int bc, double *out) {
    for (int i = 0; i < ar; i++) {
        for (int j = 0; j < bc; j++) {
            double sum = 0.0;
            for (int k = 0; k < ac; k++) {
                sum += a[i * ac + k] * b[k * bc + j];
            }
            out[i * bc + j] = sum;
        }
    }
}

// y = A(ar x ac) * x(ac).
void mat_vec_mul(const double *a, int ar, int ac, const double *x, double *out) {
    for (int i = 0; i < ar; i++) {
        double sum = 0.0;
        for (int k = 0; k < ac; k++) {
            sum += a[i * ac + k] * x[k];
        }
        out[i] = sum;
    }
}

void mat_transpose(const double *a, int r, int c, double *out) {
    for (int i = 0; i < r; i++) {
        for (int j = 0; j < c; j++) {
            out[j * r + i] = a[i * c + j];
        }
    }
}

void vec_add(const double *a, const double *b, int n, double *out) {
    for (int i = 0; i < n; i++) out[i] = a[i] + b[i];
}

void vec_sub(const double *a, const double *b, int n, double *out) {
    for (int i = 0; i < n; i++) out[i] = a[i] - b[i];
}

#define SINGULAR_EPSILON 1e-12

// Gauss-Jordan with partial pivoting. Returns 0 on success, -1 if singular.
int mat_inv(const double *elems, int n, double *inv) {
    double a[16];
    for (int i = 0; i < n * n; i++) a[i] = elems[i];
    for (int i = 0; i < n * n; i++) inv[i] = 0.0;
    for (int i = 0; i < n; i++) inv[i * n + i] = 1.0;

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
        if (max_val < SINGULAR_EPSILON) {
            return -1;
        }
        if (pivot_row != col) {
            for (int k = 0; k < n; k++) {
                double tmp = a[col * n + k];
                a[col * n + k] = a[pivot_row * n + k];
                a[pivot_row * n + k] = tmp;
                tmp = inv[col * n + k];
                inv[col * n + k] = inv[pivot_row * n + k];
                inv[pivot_row * n + k] = tmp;
            }
        }
        double pivot = a[col * n + col];
        for (int k = 0; k < n; k++) {
            a[col * n + k] /= pivot;
            inv[col * n + k] /= pivot;
        }
        for (int row = 0; row < n; row++) {
            if (row == col) continue;
            double factor = a[row * n + col];
            if (factor != 0.0) {
                for (int k = 0; k < n; k++) {
                    a[row * n + k] -= factor * a[col * n + k];
                    inv[row * n + k] -= factor * inv[col * n + k];
                }
            }
        }
    }
    return 0;
}

// x' = Fx, P' = FPF^T + Q.
void kf_predict(const double *x, const double *p, const double *f, const double *q, int n,
                 double *x_new, double *p_new) {
    mat_vec_mul(f, n, n, x, x_new);
    double ft[16];
    mat_transpose(f, n, n, ft);
    double fp[16];
    mat_mul(f, n, n, p, n, fp);
    double fpft[16];
    mat_mul(fp, n, n, ft, n, fpft);
    vec_add(fpft, q, n * n, p_new);
}

// y = z - Hx, S = HPH^T + R, K = PH^T S^-1, x' = x + Ky, P' = (I - KH)P.
// Returns 0 on success, -1 if S is singular.
int kf_update(const double *x, const double *p, const double *z, const double *h, const double *r,
              int n, int m, double *x_new, double *p_new) {
    double hx[4];
    mat_vec_mul(h, m, n, x, hx);
    double y[4];
    vec_sub(z, hx, m, y);
    double ht[16];
    mat_transpose(h, m, n, ht);
    double hp[16];
    mat_mul(h, m, n, p, n, hp);
    double hpht[16];
    mat_mul(hp, m, n, ht, m, hpht);
    double s[16];
    vec_add(hpht, r, m * m, s);
    double s_inv[16];
    if (mat_inv(s, m, s_inv) != 0) return -1;
    double pht[16];
    mat_mul(p, n, n, ht, m, pht);
    double k[16];
    mat_mul(pht, n, m, s_inv, m, k);
    double ky[16];
    mat_vec_mul(k, n, m, y, ky);
    vec_add(x, ky, n, x_new);
    double kh[16];
    mat_mul(k, n, m, h, n, kh);
    double i_minus_kh[16];
    for (int i = 0; i < n; i++) {
        for (int j = 0; j < n; j++) {
            i_minus_kh[i * n + j] = (i == j ? 1.0 : 0.0) - kh[i * n + j];
        }
    }
    mat_mul(i_minus_kh, n, n, p, n, p_new);
    return 0;
}

int main(void) {
    long n_max = 200000;
    double t = 0.0;

    double x[4] = {0.0, 0.0, 0.0, 0.0};
    double p[16] = {
        1.0, 0.0, 0.0, 0.0,
        0.0, 1.0, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
        0.0, 0.0, 0.0, 1.0,
    };
    double f[16] = {
        1.0, 0.0, 1.0, 0.0,
        0.0, 1.0, 0.0, 1.0,
        0.0, 0.0, 1.0, 0.0,
        0.0, 0.0, 0.0, 1.0,
    };
    double q[16] = {
        0.01, 0.0, 0.0, 0.0,
        0.0, 0.01, 0.0, 0.0,
        0.0, 0.0, 0.01, 0.0,
        0.0, 0.0, 0.0, 0.01,
    };
    double h[8] = {
        1.0, 0.0, 0.0, 0.0,
        0.0, 1.0, 0.0, 0.0,
    };
    double r[4] = {
        0.25, 0.0,
        0.0, 0.25,
    };

    for (long n = 0; n < n_max; n++) {
        double x1[4], p1[16];
        kf_predict(x, p, f, q, 4, x1, p1);

        double z[2] = {t, t * 0.5};

        double x2[4], p2[16];
        kf_update(x1, p1, z, h, r, 4, 2, x2, p2);

        for (int i = 0; i < 4; i++) x[i] = x2[i];
        for (int i = 0; i < 16; i++) p[i] = p2[i];
        t += 0.01;
    }
    printf("%f\n", x[0]);
    printf("%f\n", x[1]);
    return 0;
}
