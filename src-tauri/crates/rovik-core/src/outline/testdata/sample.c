#include <stdio.h>
#define MAX(a, b) ((a) > (b) ? (a) : (b))
#define LIMIT 10
typedef struct { int x; } Point;
struct node { struct node *next; };
enum color { RED, GREEN };
static int count;
int add(int a, int b);
char *name(void);
static inline int
mul(int a,
    int b)
{
    return a * b;
}
#ifdef DEBUG
void debug_log(const char *msg) {}
#endif
int main(int argc, char **argv) { return 0; }
