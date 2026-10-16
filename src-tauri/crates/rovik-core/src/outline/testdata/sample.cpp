#include <vector>
namespace app {
namespace detail { int helper(); }
template <typename T>
class Box : public Base {
public:
    Box();
    ~Box();
    T get() const { return value; }
    void set(T v);
private:
    T value;
};
template <typename T>
void Box<T>::set(T v) { value = v; }
struct Pair { int a, b; };
using Vec = std::vector<int>;
enum class Mode { On, Off };
}
int main() { return 0; }
