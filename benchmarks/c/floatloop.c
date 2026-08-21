#include <stdio.h>

int main(void) {
    double acc = 0.0;
    long i = 0;
    long n_max = 200000000;
    while (i < n_max) {
        acc = acc + 1.5;
        acc = acc * 0.999999;
        i = i + 1;
    }
    printf("%f\n", acc);
    return 0;
}
