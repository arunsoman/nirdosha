#include <stdio.h>

int main(void) {
    long n_max = 200000;
    double t = 0.0;
    double checksum = 0.0;
    for (long n = 0; n < n_max; n++) {
        double a[8] = {t, t + 1.0, t + 2.0, t + 3.0, t + 4.0, t + 5.0, t + 6.0, t + 7.0};
        double b[8] = {t + 1.0, t, t + 3.0, t + 2.0, t + 5.0, t + 4.0, t + 7.0, t + 6.0};
        double sum = 0.0;
        for (int i = 0; i < 8; i++) {
            sum += a[i] * b[i];
        }
        checksum += sum;
        t += 0.0001;
    }
    printf("%f\n", checksum);
    return 0;
}
