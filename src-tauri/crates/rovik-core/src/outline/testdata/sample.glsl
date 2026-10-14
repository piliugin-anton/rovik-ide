#version 450
struct Light { vec3 pos; };
uniform Light light;
vec3 shade(vec3 n) { return n; }
void main() { gl_FragColor = vec4(1.0); }
