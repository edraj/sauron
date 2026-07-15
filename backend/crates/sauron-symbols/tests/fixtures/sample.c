int compute_total(int n) {
  return n * 2 + 1;
}
int helper_add(int a, int b) {
  return compute_total(a) + b;
}
int main(void) {
  return helper_add(3, 4);
}
