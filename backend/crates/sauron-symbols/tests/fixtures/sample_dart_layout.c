/* Stands in for a Dart --split-debug-info file: the isolate/VM instruction
 * symbols mark where the code that trace offsets are relative to begins. */
__asm__(".text\n"
        ".globl _kDartVmSnapshotInstructions\n"
        "_kDartVmSnapshotInstructions:\n"
        "nop\n"
        ".globl _kDartIsolateSnapshotInstructions\n"
        "_kDartIsolateSnapshotInstructions:\n");
int load_user(int x) { return x + 1; }
int save_order(int x) { return x * 2; }
int main(void) { return load_user(1) + save_order(2); }
