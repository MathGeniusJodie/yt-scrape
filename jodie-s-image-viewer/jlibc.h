
// https://github.com/runtimejs/musl-libc/blob/master/crt/x86_64/crt1.s

__attribute__((naked)) void _start(){
	asm(
		"xorq %rbp,%rbp\n"    // zero
		"movq 0(%rsp),%rdi\n" // argc
		"leaq 8(%rsp),%rsi\n" // argv
		"lea 16(%rsp,%rdi,8), %rdx\n" // envp
		"andq $-16, %rsp\n"   // align to 16 bytes
		"call main\n"
		"movq %rax,%rdi\n"
		"movq $60,%rax\n"     // exit
		"syscall"
	);
}

#define size_t long unsigned int

// https://blog.rchapman.org/posts/Linux_System_Call_Table_for_x86_64/
// https://gcc.gnu.org/onlinedocs/gcc/Machine-Constraints.html#Machine-Constraints

size_t read(int fd, void* buf, size_t size) {
  	size_t result;
  	asm volatile(
  		"syscall" :
  		"=a"(result) :
  		"0"(0), "D"(fd), "S"(buf), "d"(size) :
  		"rcx", "r11", "memory"
	);
  	return result;
}
size_t write(int fd, void* buf, size_t size) {
  	size_t result;
  	asm volatile(
  		"syscall" :
  		"=a"(result) :
  		"0"(1), "D"(fd), "S"(buf), "d"(size) :
  		"rcx", "r11", "memory"
	);
  	return result;
}

void exit(int status){
	asm volatile(
		"syscall" : // a=RAX, D=EDI, S=RSI, d=RDX
		: "a"(60), "D"(status)
		: "rcx", "r11" , "memory"
	);
}

// https://code.woboq.org/llvm/clang/lib/Headers/intrin.h.html

void * memcpy(void *restrict dest, const void *src, size_t n){
	asm volatile(
		"rep movsb"
		: "+D" (dest) , "+c"(n), "+S"(src) 
		:
		: "memory");
	return dest;
}

void * memset(void *dest, int c, size_t n) {
	asm volatile (
    	"rep stosb"
    	: "+D" (dest), "+c" (n) : "a" (c)
    	: "memory");
	return dest;
}

// shamelessly stolen from musl
size_t strlen(const char *s){
	const char *a = s;
	for (; *s; s++);
	return s-a;
}
//https://medium.com/@ophirharpaz/a-summary-of-x86-string-instructions-87566a28c20c
size_t strlen2(const char *s){
	size_t rcx = 4294967295;
	asm volatile(
		"repne scasb":"+c"(rcx): "D" (s) , "a"(0) : "flags"
	);
	return 4294967295-1-rcx;
}
