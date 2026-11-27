// Does a Direct3D 12 runtime execute work? Clears a texture to a known
// colour, copies it to a read-back buffer, waits on a fence, and checks the
// pixel; then checks that GPU timestamps advance; then builds the engine's
// two-plane composer (compose.rs) and checks one composed pixel. For Wine + D3DMetal.
// Build: x86_64-w64-mingw32-clang++ -O1 mac/d3d12-probe.cpp -ld3d12 -ldxgi -ld3dcompiler -o d3d12-probe.exe
#include <d3d12.h>
#include <d3dcompiler.h>
#include <string.h>
#include <dxgi1_4.h>
#include <stdio.h>
#include <stdlib.h>
#include <utility>
#include <windows.h>

static int failures;
#define CHECK(ok, ...) do { printf("%s ", (ok) ? "ok  " : "FAIL"); printf(__VA_ARGS__); printf("\n"); fflush(stdout); if (!(ok)) failures++; } while (0)

static D3D12_HEAP_PROPERTIES heap(D3D12_HEAP_TYPE type) {
    D3D12_HEAP_PROPERTIES h = {};
    h.Type = type;
    return h;
}


// The composer's shader, as in crates/victory-gpu/src/compose.rs.
static const char COMPOSE[] = R"hlsl(
#define RS "DescriptorTable(SRV(t0, numDescriptors=2), visibility=SHADER_VISIBILITY_PIXEL), StaticSampler(s0, filter=FILTER_MIN_MAG_MIP_LINEAR, addressU=TEXTURE_ADDRESS_CLAMP, addressV=TEXTURE_ADDRESS_CLAMP)"
Texture2D<float4> scene : register(t0);
Texture2D<float4> over : register(t1);
SamplerState smooth_ : register(s0);
struct V { float4 at : SV_Position; float2 uv : TEXCOORD0; };
[RootSignature(RS)]
V vs(uint id : SV_VertexID) {
    V v;
    v.uv = float2((id << 1) & 2, id & 2);
    v.at = float4(v.uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return v;
}
[RootSignature(RS)]
float4 ps(V v) : SV_Target {
    float4 s = scene.SampleLevel(smooth_, v.uv, 0);
    float4 o = over.SampleLevel(smooth_, v.uv, 0);
    return float4(saturate(o.rgb + s.rgb * (1 - saturate(o.a))), 1);
}
)hlsl";

static ID3DBlob *compile(const char *entry, const char *target) {
    ID3DBlob *code = nullptr, *errors = nullptr;
    HRESULT hr = D3DCompile(COMPOSE, sizeof COMPOSE - 1, "compose", nullptr, nullptr, entry, target, 0, 0, &code, &errors);
    CHECK(SUCCEEDED(hr), "composer %s compiles as %s (hr %08lx)%s%s", entry, target, hr, errors ? ": " : "",
          errors ? (const char *)errors->GetBufferPointer() : "");
    return code;
}

static bool has_chunk(ID3DBlob *b, const char *fourcc) {
    const unsigned char *p = (const unsigned char *)b->GetBufferPointer();
    for (size_t i = 0; i + 4 <= b->GetBufferSize(); i++)
        if (!memcmp(p + i, fourcc, 4)) return true;
    return false;
}

int main() {
    ID3D12Device *dev = nullptr;
    HRESULT hr = D3D12CreateDevice(nullptr, D3D_FEATURE_LEVEL_12_0, __uuidof(ID3D12Device), (void **)&dev);
    CHECK(SUCCEEDED(hr), "device (hr %08lx)", hr);
    if (!dev) return 1;

    D3D12_COMMAND_QUEUE_DESC qd = {};
    qd.Type = D3D12_COMMAND_LIST_TYPE_DIRECT;
    ID3D12CommandQueue *queue;
    dev->CreateCommandQueue(&qd, __uuidof(ID3D12CommandQueue), (void **)&queue);
    ID3D12CommandAllocator *alloc;
    dev->CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT, __uuidof(ID3D12CommandAllocator), (void **)&alloc);
    ID3D12GraphicsCommandList *list;
    dev->CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, alloc, nullptr, __uuidof(ID3D12GraphicsCommandList), (void **)&list);

    D3D12_RESOURCE_DESC td = {};
    td.Dimension = D3D12_RESOURCE_DIMENSION_TEXTURE2D;
    td.Width = 64; td.Height = 64; td.DepthOrArraySize = 1; td.MipLevels = 1;
    td.Format = DXGI_FORMAT_R8G8B8A8_UNORM; td.SampleDesc.Count = 1;
    td.Flags = D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET;
    D3D12_HEAP_PROPERTIES hd = heap(D3D12_HEAP_TYPE_DEFAULT);
    ID3D12Resource *tex;
    hr = dev->CreateCommittedResource(&hd, D3D12_HEAP_FLAG_NONE, &td, D3D12_RESOURCE_STATE_RENDER_TARGET, nullptr, __uuidof(ID3D12Resource), (void **)&tex);
    CHECK(SUCCEEDED(hr), "render target (hr %08lx)", hr);

    D3D12_RESOURCE_DESC bd = {};
    bd.Dimension = D3D12_RESOURCE_DIMENSION_BUFFER;
    bd.Width = 256 * 64; bd.Height = 1; bd.DepthOrArraySize = 1; bd.MipLevels = 1;
    bd.SampleDesc.Count = 1; bd.Layout = D3D12_TEXTURE_LAYOUT_ROW_MAJOR;
    D3D12_HEAP_PROPERTIES hr_ = heap(D3D12_HEAP_TYPE_READBACK);
    ID3D12Resource *back;
    dev->CreateCommittedResource(&hr_, D3D12_HEAP_FLAG_NONE, &bd, D3D12_RESOURCE_STATE_COPY_DEST, nullptr, __uuidof(ID3D12Resource), (void **)&back);

    D3D12_DESCRIPTOR_HEAP_DESC rd = {};
    rd.Type = D3D12_DESCRIPTOR_HEAP_TYPE_RTV; rd.NumDescriptors = 1;
    ID3D12DescriptorHeap *rtvs;
    dev->CreateDescriptorHeap(&rd, __uuidof(ID3D12DescriptorHeap), (void **)&rtvs);
    D3D12_CPU_DESCRIPTOR_HANDLE rtv = rtvs->GetCPUDescriptorHandleForHeapStart();
    dev->CreateRenderTargetView(tex, nullptr, rtv);

    D3D12_QUERY_HEAP_DESC qh = {};
    qh.Type = D3D12_QUERY_HEAP_TYPE_TIMESTAMP; qh.Count = 2;
    ID3D12QueryHeap *queries;
    dev->CreateQueryHeap(&qh, __uuidof(ID3D12QueryHeap), (void **)&queries);

    list->EndQuery(queries, D3D12_QUERY_TYPE_TIMESTAMP, 0);
    const float colour[4] = {0.2f, 0.4f, 0.6f, 1.0f};
    list->ClearRenderTargetView(rtv, colour, 0, nullptr);
    D3D12_RESOURCE_BARRIER b = {};
    b.Type = D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
    b.Transition.pResource = tex;
    b.Transition.StateBefore = D3D12_RESOURCE_STATE_RENDER_TARGET;
    b.Transition.StateAfter = D3D12_RESOURCE_STATE_COPY_SOURCE;
    list->ResourceBarrier(1, &b);
    D3D12_TEXTURE_COPY_LOCATION dst = {}, src = {};
    dst.pResource = back; dst.Type = D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT;
    dst.PlacedFootprint.Footprint.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    dst.PlacedFootprint.Footprint.Width = 64; dst.PlacedFootprint.Footprint.Height = 64;
    dst.PlacedFootprint.Footprint.Depth = 1; dst.PlacedFootprint.Footprint.RowPitch = 256;
    src.pResource = tex; src.Type = D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX;
    list->CopyTextureRegion(&dst, 0, 0, 0, &src, nullptr);
    list->EndQuery(queries, D3D12_QUERY_TYPE_TIMESTAMP, 1);
    D3D12_RESOURCE_DESC sd = bd; sd.Width = 16;
    ID3D12Resource *stamps;
    dev->CreateCommittedResource(&hr_, D3D12_HEAP_FLAG_NONE, &sd, D3D12_RESOURCE_STATE_COPY_DEST, nullptr, __uuidof(ID3D12Resource), (void **)&stamps);
    list->ResolveQueryData(queries, D3D12_QUERY_TYPE_TIMESTAMP, 0, 2, stamps, 0);
    list->Close();
    ID3D12CommandList *lists[] = {list};
    queue->ExecuteCommandLists(1, lists);

    ID3D12Fence *fence;
    dev->CreateFence(0, D3D12_FENCE_FLAG_NONE, __uuidof(ID3D12Fence), (void **)&fence);
    HANDLE done = CreateEventA(nullptr, FALSE, FALSE, nullptr);
    queue->Signal(fence, 1);
    fence->SetEventOnCompletion(1, done);
    DWORD waited = WaitForSingleObject(done, 5000);
    CHECK(waited == WAIT_OBJECT_0 && fence->GetCompletedValue() == 1, "fence signalled after the work (value %llu)",
          (unsigned long long)fence->GetCompletedValue());

    unsigned char *px = nullptr;
    back->Map(0, nullptr, (void **)&px);
    CHECK(px && px[0] == 51 && px[1] == 102 && px[2] == 153 && px[3] == 255,
          "cleared pixel read back: %u %u %u %u (want 51 102 153 255)", px ? px[0] : 0, px ? px[1] : 0, px ? px[2] : 0, px ? px[3] : 0);
    UINT64 *t = nullptr, freq = 0;
    stamps->Map(0, nullptr, (void **)&t);
    queue->GetTimestampFrequency(&freq);
    CHECK(t && t[1] > t[0], "GPU timestamps advance (%llu -> %llu at %llu Hz)", t ? (unsigned long long)t[0] : 0,
          t ? (unsigned long long)t[1] : 0, (unsigned long long)freq);

    // The two-plane composer: scene cleared to (0.2, 0.4, 0.6), interface
    // plane to (0.5, 0, 0, 0.5) premultiplied; composed: 0.5 + 0.2 * 0.5 = 0.6,
    // 0.4 * 0.5 = 0.2, 0.6 * 0.5 = 0.3, that is 153 51 77 255.
    ID3DBlob *vs = compile("vs", "vs_5_1"), *ps = compile("ps", "ps_5_1");
    if (vs && ps) {
        CHECK(has_chunk(vs, "RTS0"), "the compiled vertex shader holds its root signature (RTS0)");
        ID3D12RootSignature *embedded = nullptr;
        hr = dev->CreateRootSignature(0, vs->GetBufferPointer(), vs->GetBufferSize(), __uuidof(ID3D12RootSignature), (void **)&embedded);
        CHECK(SUCCEEDED(hr), "root signature from the compiled shader (hr %08lx)", hr);

        D3D12_DESCRIPTOR_RANGE range = {D3D12_DESCRIPTOR_RANGE_TYPE_SRV, 2, 0, 0, 0};
        D3D12_ROOT_PARAMETER param = {};
        param.ParameterType = D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE;
        param.DescriptorTable = {1, &range};
        param.ShaderVisibility = D3D12_SHADER_VISIBILITY_PIXEL;
        D3D12_STATIC_SAMPLER_DESC sampler = {};
        sampler.Filter = D3D12_FILTER_MIN_MAG_MIP_LINEAR;
        sampler.AddressU = sampler.AddressV = sampler.AddressW = D3D12_TEXTURE_ADDRESS_MODE_CLAMP;
        sampler.MaxLOD = D3D12_FLOAT32_MAX;
        sampler.ShaderVisibility = D3D12_SHADER_VISIBILITY_ALL;
        D3D12_ROOT_SIGNATURE_DESC rsd = {1, &param, 1, &sampler, D3D12_ROOT_SIGNATURE_FLAG_NONE};
        ID3DBlob *serialized = nullptr;
        hr = D3D12SerializeRootSignature(&rsd, D3D_ROOT_SIGNATURE_VERSION_1, &serialized, nullptr);
        ID3D12RootSignature *root = nullptr;
        if (SUCCEEDED(hr))
            hr = dev->CreateRootSignature(0, serialized->GetBufferPointer(), serialized->GetBufferSize(), __uuidof(ID3D12RootSignature), (void **)&root);
        CHECK(SUCCEEDED(hr), "root signature serialized from its description (hr %08lx)", hr);

        D3D12_GRAPHICS_PIPELINE_STATE_DESC pd = {};
        pd.pRootSignature = root;
        pd.VS = {vs->GetBufferPointer(), vs->GetBufferSize()};
        pd.PS = {ps->GetBufferPointer(), ps->GetBufferSize()};
        pd.SampleMask = UINT_MAX;
        pd.RasterizerState.FillMode = D3D12_FILL_MODE_SOLID;
        pd.RasterizerState.CullMode = D3D12_CULL_MODE_NONE;
        pd.PrimitiveTopologyType = D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE;
        pd.NumRenderTargets = 1;
        pd.RTVFormats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
        pd.SampleDesc.Count = 1;
        pd.BlendState.RenderTarget[0].RenderTargetWriteMask = D3D12_COLOR_WRITE_ENABLE_ALL;
        ID3D12PipelineState *pso = nullptr;
        hr = root ? dev->CreateGraphicsPipelineState(&pd, __uuidof(ID3D12PipelineState), (void **)&pso) : E_FAIL;
        CHECK(SUCCEEDED(hr), "composer pipeline (hr %08lx)", hr);

        if (pso) {
            ID3D12Resource *planes[2];
            D3D12_DESCRIPTOR_HEAP_DESC prd = {D3D12_DESCRIPTOR_HEAP_TYPE_RTV, 2};
            ID3D12DescriptorHeap *prtv;
            dev->CreateDescriptorHeap(&prd, __uuidof(ID3D12DescriptorHeap), (void **)&prtv);
            D3D12_DESCRIPTOR_HEAP_DESC srd = {D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV, 2, D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE};
            ID3D12DescriptorHeap *srvs;
            dev->CreateDescriptorHeap(&srd, __uuidof(ID3D12DescriptorHeap), (void **)&srvs);
            UINT rstep = dev->GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV);
            UINT sstep = dev->GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV);
            const float clears[2][4] = {{0.2f, 0.4f, 0.6f, 1.0f}, {0.5f, 0.0f, 0.0f, 0.5f}};
            alloc->Reset();
            list->Reset(alloc, nullptr);
            for (int i = 0; i < 2; i++) {
                dev->CreateCommittedResource(&hd, D3D12_HEAP_FLAG_NONE, &td, D3D12_RESOURCE_STATE_RENDER_TARGET, nullptr, __uuidof(ID3D12Resource), (void **)&planes[i]);
                D3D12_CPU_DESCRIPTOR_HANDLE at = prtv->GetCPUDescriptorHandleForHeapStart();
                at.ptr += i * rstep;
                dev->CreateRenderTargetView(planes[i], nullptr, at);
                list->ClearRenderTargetView(at, clears[i], 0, nullptr);
                D3D12_RESOURCE_BARRIER pb = {};
                pb.Type = D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
                pb.Transition.pResource = planes[i];
                pb.Transition.StateBefore = D3D12_RESOURCE_STATE_RENDER_TARGET;
                pb.Transition.StateAfter = D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE;
                list->ResourceBarrier(1, &pb);
                D3D12_SHADER_RESOURCE_VIEW_DESC sd = {};
                sd.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
                sd.ViewDimension = D3D12_SRV_DIMENSION_TEXTURE2D;
                sd.Shader4ComponentMapping = D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING;
                sd.Texture2D.MipLevels = 1;
                D3D12_CPU_DESCRIPTOR_HANDLE sat = srvs->GetCPUDescriptorHandleForHeapStart();
                sat.ptr += i * sstep;
                dev->CreateShaderResourceView(planes[i], &sd, sat);
            }
            std::swap(b.Transition.StateBefore, b.Transition.StateAfter);
            list->ResourceBarrier(1, &b);
            list->OMSetRenderTargets(1, &rtv, FALSE, nullptr);
            list->SetDescriptorHeaps(1, &srvs);
            list->SetGraphicsRootSignature(root);
            list->SetPipelineState(pso);
            list->SetGraphicsRootDescriptorTable(0, srvs->GetGPUDescriptorHandleForHeapStart());
            list->IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            D3D12_VIEWPORT vp = {0, 0, 64, 64, 0, 1};
            D3D12_RECT sc = {0, 0, 64, 64};
            list->RSSetViewports(1, &vp);
            list->RSSetScissorRects(1, &sc);
            list->DrawInstanced(3, 1, 0, 0);
            std::swap(b.Transition.StateBefore, b.Transition.StateAfter);
            list->ResourceBarrier(1, &b);
            list->CopyTextureRegion(&dst, 0, 0, 0, &src, nullptr);
            list->Close();
            queue->ExecuteCommandLists(1, lists);
            queue->Signal(fence, 2);
            fence->SetEventOnCompletion(2, done);
            WaitForSingleObject(done, 5000);
            px = nullptr;
            back->Map(0, nullptr, (void **)&px);
            unsigned char *c = px ? px + 32 * 256 + 32 * 4 : nullptr;
            CHECK(c && abs(c[0] - 153) <= 1 && abs(c[1] - 51) <= 1 && abs(c[2] - 77) <= 1 && c[3] == 255,
                  "composed pixel: %u %u %u %u (want 153 51 77 255)", c ? c[0] : 0, c ? c[1] : 0, c ? c[2] : 0, c ? c[3] : 0);
        }
    }
    printf("%s (%d failed)\n", failures ? "D3D12 PROBE FAILED" : "D3D12 PROBE PASSED", failures);
    return failures != 0;
}
