using System;
namespace App.Core
{
    [Serializable]
    public class Service : IService
    {
        private int count;
        public Service() {}
        public string Name { get; set; }
        public async Task<int> RunAsync(int x) { return x; }
        public event EventHandler Changed;
        public static Service operator +(Service a, Service b) => a;
        private record struct Point(int X, int Y);
    }
    public interface IService { void Run(); }
    public enum Kind { A, B }
    public delegate void Handler(int x);
}
