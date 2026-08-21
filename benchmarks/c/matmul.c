#include <stdio.h>

int main(void) {
    long n_max = 200000;
    double t = 0.0;
    double checksum = 0.0;
    for (long n = 0; n < n_max; n++) {
        double a[4][4] = {
            {t, t + 1.0, t + 2.0, t + 3.0},
            {t + 4.0, t + 5.0, t + 6.0, t + 7.0},
            {t + 8.0, t + 9.0, t + 10.0, t + 11.0},
            {t + 12.0, t + 13.0, t + 14.0, t + 15.0},
        };
        double b[4][4] = {
            {t + 1.0, t, t + 3.0, t + 2.0},
            {t + 5.0, t + 4.0, t + 7.0, t + 6.0},
            {t + 9.0, t + 8.0, t + 11.0, t + 10.0},
            {t + 13.0, t + 12.0, t + 15.0, t + 14.0},
        };
        double c[4][4];
        for (int i = 0; i < 4; i++) {
            for (int j = 0; j < 4; j++) {
                double sum = 0.0;
                for (int k = 0; k < 4; k++) {
                    sum += a[i][k] * b[k][j];
                }
                c[i][j] = sum;
            }
        }
        checksum += c[0][0];
        t += 0.0001;
    }
    printf("%f\n", checksum);
    return 0;
}
