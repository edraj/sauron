__attribute__((always_inline)) static inline int scale(int x) {
  return x * 7 + 2;
}
int outer(int y) {
  return scale(y) - 1;
}
int main(void) {
  return outer(4);
}
