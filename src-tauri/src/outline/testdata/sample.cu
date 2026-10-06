#include <cuda.h>
__global__ void kernel(float *a, int n) { int i = threadIdx.x; }
__device__ float helper(float x) { return x; }
struct Params { int n; };
int main() { return 0; }
