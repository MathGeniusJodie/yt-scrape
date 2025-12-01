#include <stdio.h>
//#define printf(...) (0)
#include <stdlib.h>
#include <math.h>
#include <unistd.h>
#include <string.h>

static inline void stringreverse(char * str,char * end){
    while (str < end){
        char temp = *str;
      *str = *end;
      *end = temp;
      str++;
      end--;
    }
}
/*
void itoa(int i, char a[], int base) {
	char* l=a;
    if(i<0){
      i=-i;
      *l='-';
      l+=1;
      a+=1;
    }
	while(i){
      int n = i%base;
      *l = n + (n<10?'0':'a'-10);
      l+=1;
      i/=base;
    }
    *l='\0';
	stringreverse(a,l-1);
}


void itoalength(int i,char a[],int l){
  if(i<0){
    i=-i;
  }
  a[l]='\0';
  while (l){
    l-=1;
    int n = i % 10;
    a[l] = n + '0';
    i /= 10;
  }
}

*/

float my_log10f(float a) {
    float b;
    __asm__ volatile(
    "fldlg2\n"
    "flds %1\n"
    "fyl2x\n"
    "fsts %0":"=m"(b):"m"(a)
    );   
    return b;
}

#define min(a,b) (((a)<(b))?(a):(b))
#define max(a,b) (((a)>(b))?(a):(b))

#define cuint const unsigned int
#define cint const int
#define cfloat const float
#define uint unsigned int

size_t float_to_string(const float f, char a[]) {

    const uint precision_digits = 6;
    const float unsignedf = fabsf(f);

    // `proxy` is the integer we actually print
    // it is offset to always be the same length
    const uint proxy_length = precision_digits + 1;
    const int proxy_offset = precision_digits - (int) my_log10f(unsignedf);
    uint proxy = unsignedf * __builtin_exp10f(proxy_offset);

	// if `proxy_offset` is negative,
    // meaning the number was divided,
    // add trailing zeroes to multiply it back up
    uint trailing_zeroes_length = max(-proxy_offset, 0);

    // proxy length and leading zeroes
	// the +1 is for the zero before the 
	// decimal point in numbers smaller than 1
    uint main_loop_length = max(proxy_length, proxy_offset + 1);

    const uint has_minus_sign = f < 0.f;
    const uint has_decimal_point = proxy_offset > 0;

    const uint length = trailing_zeroes_length
    	+ main_loop_length
    	+ has_decimal_point
    	+ has_minus_sign
    	// null byte
    	+ 1u;

	// -1 is for the null byte
    const uint decimal_position = length - proxy_offset - 1u;

	// write sting starting from the end
    uint i = length;

    a[i--] = '\0';

    while (trailing_zeroes_length--) a[i--] = '0';

    while (main_loop_length--) {
        a[i--] = (proxy % 10) + '0';
        proxy /= 10;
        // make space for decimal point without branch
        i -= i == decimal_position;
    }
    if (has_decimal_point)
    	a[decimal_position] = '.';

    if (has_minus_sign) a[0] = '-';

    return length;
}


int main() {

	float tests[] = {};
	
    float floatnum = -34.56789f*1000.00f;
    char buffer[50];

    size_t length = float_to_string(floatnum,buffer);

    memcpy(buffer+length,"\n",1);

    write(1,buffer,length+1);
    printf("%f\n",floatnum);
    
    return 0;
}
  
