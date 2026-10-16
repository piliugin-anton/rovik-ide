struct VSInput { float3 pos : POSITION; };
cbuffer Constants : register(b0) { float4x4 mvp; };
float4 VSMain(VSInput input) : SV_POSITION { return float4(input.pos, 1); }
namespace util { float sq(float x) { return x * x; } }
