// Does a Direct3D 12 runtime execute work? Clears a texture to a known
// colour, copies it to a read-back buffer, waits on a fence, and checks the
// pixel; then checks that GPU timestamps advance. For Wine + D3DMetal.
// Build: x86_64-w64-mingw32-clang++ -O1 mac/d3d12-probe.cpp -ld3d12 -ldxgi -o d3d12-probe.exe
#include <d3d12.h>
#include <dxgi1_4.h>
#include <stdio.h>
#include <windows.h>

static int failures;
#define CHECK(ok, ...) do { printf("%s ", (ok) ? "ok  " : "FAIL"); printf(__VA_ARGS__); printf("\n"); fflush(stdout); if (!(ok)) failures++; } while (0)

static D3D12_HEAP_PROPERTIES heap(D3D12_HEAP_TYPE type) {
    D3D12_HEAP_PROPERTIES h = {};
    h.Type = type;
    return h;
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
    printf("%s (%d failed)\n", failures ? "D3D12 PROBE FAILED" : "D3D12 PROBE PASSED", failures);
    return failures != 0;
}
