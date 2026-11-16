#include <d3d11.h>
#include <d3dcompiler.h>
#include <dxgi1_2.h>
#include <wrl/client.h>
#include <chrono>
#include <array>
#include <cstdlib>
#include <filesystem>
#include <iostream>
#include <stdexcept>
#include <string>
#include <thread>

using Microsoft::WRL::ComPtr;

static void Check(HRESULT result) {
  if (FAILED(result)) throw std::runtime_error("D3D failure: " + std::to_string(result));
}

int main(int argc, char** argv) {
  try {
    const int seconds = argc > 1 ? std::atoi(argv[1]) : 30;
    if (seconds < 1 || seconds > 60 || argc != 3) return 2;
    const std::filesystem::path stop(argv[2]);
    ComPtr<IDXGIFactory1> factory;
    Check(CreateDXGIFactory1(IID_PPV_ARGS(&factory)));
    ComPtr<IDXGIAdapter1> adapter;
    SIZE_T largest = 0;
    for (UINT index = 0;; ++index) {
      ComPtr<IDXGIAdapter1> candidate;
      const HRESULT enumerated = factory->EnumAdapters1(index, &candidate);
      if (enumerated == DXGI_ERROR_NOT_FOUND) break;
      Check(enumerated);
      DXGI_ADAPTER_DESC1 desc{};
      Check(candidate->GetDesc1(&desc));
      if (!(desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE) && (!adapter || desc.DedicatedVideoMemory > largest)) {
        largest = desc.DedicatedVideoMemory;
        adapter = candidate;
      }
    }
    if (!adapter) return 3;
    DXGI_ADAPTER_DESC1 desc{};
    Check(adapter->GetDesc1(&desc));
    std::wcout << L"GPU: " << desc.Description << L"; duration: " << seconds << L" seconds\n";
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext> context;
    Check(D3D11CreateDevice(adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, nullptr, 0,
                           D3D11_SDK_VERSION, &device, nullptr, &context));
    constexpr char shader[] = R"(
RWStructuredBuffer<float4> output : register(u0);
[numthreads(256,1,1)] void main(uint3 id : SV_DispatchThreadID) {
  float4 a = float4(.1,.2,.3,.4) + (id.x & 255) * .001;
  float4 b = a.yzwx, c = a.zwxy, d = a.wxyz;
  [loop] for (uint i = 0; i < 8192; ++i) {
    a = mad(a, .99991, b * .00009);
    b = mad(b, .99993, c * .00007);
    c = mad(c, .99989, d * .00011);
    d = mad(d, .99987, a * .00013);
  }
  output[id.x] = a+b+c+d;
})";
    ComPtr<ID3DBlob> bytecode, errors;
    Check(D3DCompile(shader, sizeof(shader), nullptr, nullptr, nullptr, "main", "cs_5_0",
                     D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &bytecode, &errors));
    ComPtr<ID3D11ComputeShader> compute;
    Check(device->CreateComputeShader(bytecode->GetBufferPointer(), bytecode->GetBufferSize(), nullptr, &compute));
    D3D11_BUFFER_DESC buffer_desc{};
    buffer_desc.ByteWidth = 1024 * 256 * 16;
    buffer_desc.BindFlags = D3D11_BIND_UNORDERED_ACCESS;
    buffer_desc.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED;
    buffer_desc.StructureByteStride = 16;
    ComPtr<ID3D11Buffer> buffer;
    Check(device->CreateBuffer(&buffer_desc, nullptr, &buffer));
    ComPtr<ID3D11UnorderedAccessView> view;
    Check(device->CreateUnorderedAccessView(buffer.Get(), nullptr, &view));
    D3D11_QUERY_DESC query_desc{D3D11_QUERY_EVENT, 0};
    // Two batches keep CPU polling gaps from being mistaken for spare GPU capacity.
    std::array<ComPtr<ID3D11Query>, 2> queries;
    for (auto& query : queries) Check(device->CreateQuery(&query_desc, &query));
    context->CSSetShader(compute.Get(), nullptr, 0);
    auto* target = view.Get();
    context->CSSetUnorderedAccessViews(0, 1, &target, nullptr);
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(seconds);
    unsigned batches = 0;
    while (std::chrono::steady_clock::now() < deadline && !std::filesystem::exists(stop)) {
      const auto current = batches % queries.size();
      for (int i = 0; i < 16; ++i) context->Dispatch(1024, 1, 1);
      context->End(queries[current].Get());
      context->Flush();
      ++batches;
      if (batches < queries.size()) continue;
      const auto waiting = batches % queries.size();
      const auto submitted = std::chrono::steady_clock::now();
      for (;;) {
        const HRESULT result = context->GetData(queries[waiting].Get(), nullptr, 0, 0);
        if (result == S_OK) break;
        Check(result);
        Check(device->GetDeviceRemovedReason());
        if (std::chrono::steady_clock::now() - submitted > std::chrono::seconds(2)) {
          throw std::runtime_error("GPU batch exceeded the probe deadline");
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
      }
    }
    context->ClearState();
    std::cout << "Completed GPU batches: " << batches << '\n';
    return 0;
  } catch (const std::exception& error) {
    std::cerr << error.what() << '\n';
    return 1;
  }
}
